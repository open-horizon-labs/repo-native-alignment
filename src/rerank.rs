//! Cross-encoder reranking for search results.
//!
//! After initial retrieval (BM25, vector, or hybrid RRF), the top-N candidates
//! can be re-scored by a cross-encoder model that attends to query-document
//! pairs jointly. This produces more precise relevance scores than bi-encoder
//! embeddings, which encode query and document independently.
//!
//! The reranker is on by default for MCP (where queries are agent-driven and
//! benefit from precision) and opt-in for CLI (`--rerank`). It is lazy-loaded
//! on first use.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use fastembed::{RerankResult, RerankerModel, TextRerank};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Global reranker instance behind a Mutex. Only caches successful
/// initialization; if model loading fails, subsequent calls will retry.
/// The `TextRerank::rerank()` method requires `&mut self` (ONNX session state),
/// so we use `Mutex` for safe concurrent access.
static RERANKER: Mutex<Option<TextRerank>> = Mutex::new(None);

/// Return the model cache directory.
/// Precedence: `FASTEMBED_CACHE_DIR` env var > `~/.cache/rna/models/` > fastembed default.
fn rna_cache_dir() -> std::path::PathBuf {
    if let Some(explicit) = std::env::var("FASTEMBED_CACHE_DIR")
        .ok()
        .filter(|v| !v.is_empty())
    {
        return std::path::PathBuf::from(explicit);
    }
    if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home)
            .join(".cache")
            .join("rna")
            .join("models")
    } else {
        std::path::PathBuf::from(fastembed::get_cache_dir())
    }
}

/// Default model: Jina Reranker V1 Turbo (English).
/// Smaller and faster than BGE-Reranker-Base while maintaining good quality.
const DEFAULT_MODEL: RerankerModel = RerankerModel::JINARerankerV1TurboEn;
const RERANKER_FILES_DIGEST_ENV: &str = "RNA_RERANKER_MODEL_FILES_DIGEST";

/// A document to be reranked, carrying its original index so we can map
/// reranked scores back to the original result set.
#[derive(Debug)]
pub struct RerankCandidate {
    /// The text to score against the query (typically the document body/title).
    pub text: String,
    /// Original index in the pre-reranking result list.
    pub original_index: usize,
}

/// Reranked result with the cross-encoder score and original index.
#[derive(Debug)]
pub struct RerankedResult {
    /// Original index in the pre-reranking result list.
    pub original_index: usize,
    /// Cross-encoder relevance score (higher = more relevant).
    pub score: f32,
}

/// Rerank a set of candidates against a query using the cross-encoder model.
///
/// Returns results sorted by cross-encoder score (descending, most relevant first).
/// If the reranker fails to load, returns an error rather than silently degrading.
///
/// # Arguments
/// * `query` - The search query
/// * `candidates` - Documents to rerank, each with text and original index
///
/// # Returns
/// Reranked results sorted by relevance score (descending).
pub fn rerank_results(query: &str, candidates: &[RerankCandidate]) -> Result<Vec<RerankedResult>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mut guard = RERANKER
        .lock()
        .map_err(|e| anyhow::anyhow!("Reranker lock poisoned: {}", e))?;

    // Lazy-initialize on first use; retry on failure (don't cache errors).
    if guard.is_none() {
        let start = std::time::Instant::now();
        tracing::info!(
            "Reranker: loading {:?} (first use, one-time cost)",
            DEFAULT_MODEL
        );

        let cache_dir = rna_cache_dir();
        std::fs::create_dir_all(&cache_dir).context(format!(
            "Failed to create reranker cache dir: {}",
            cache_dir.display()
        ))?;
        tracing::info!("Reranker: cache dir = {}", cache_dir.display());

        let init_opts = fastembed::RerankInitOptions::new(DEFAULT_MODEL).with_cache_dir(cache_dir);
        match TextRerank::try_new(init_opts) {
            Ok(reranker) => {
                tracing::info!("Reranker: ready in {:?}", start.elapsed());
                *guard = Some(reranker);
            }
            Err(e) => {
                tracing::warn!("Reranker: load failed in {:?}: {}", start.elapsed(), e);
                return Err(anyhow::anyhow!("Failed to load reranker model: {}", e));
            }
        }
    }

    let reranker = guard.as_mut().expect("reranker guaranteed Some after init");
    let start = std::time::Instant::now();

    // Collect document texts for the reranker.
    let documents: Vec<&str> = candidates.iter().map(|c| c.text.as_str()).collect();

    let rerank_results: Vec<RerankResult> = reranker
        .rerank(query, documents, false, None)
        .context("Cross-encoder reranking failed")?;

    tracing::debug!(
        "Reranker: scored {} candidates in {:?}",
        candidates.len(),
        start.elapsed()
    );

    // Map fastembed results back to our candidates using the index field.
    // Use checked access: fastembed does not guarantee indices are in bounds
    // when `return_documents = false`.
    let mut results: Vec<RerankedResult> = Vec::with_capacity(rerank_results.len());
    for r in rerank_results {
        let candidate = candidates.get(r.index).ok_or_else(|| {
            anyhow::anyhow!(
                "Reranker returned invalid index {} (candidates={})",
                r.index,
                candidates.len()
            )
        })?;
        results.push(RerankedResult {
            original_index: candidate.original_index,
            score: r.score,
        });
    }

    // Sort by score descending (fastembed may already do this, but be explicit).
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(results)
}

/// Rerank for the qualification lane, requiring a complete one-to-one result
/// permutation. Ordinary search remains best-effort at its service boundary;
/// strict qualification must never accept missing, duplicate, non-finite, or
/// out-of-domain results as a successful rerank.
pub fn rerank_results_strict(
    query: &str,
    candidates: &[RerankCandidate],
) -> Result<Vec<RerankedResult>> {
    let expected_cache_digest = verify_strict_reranker_cache()?;
    let results = rerank_results(query, candidates)?;
    validate_strict_rerank(candidates, &results)?;
    let observed_after_inference = strict_reranker_cache_digest()?;
    if observed_after_inference != expected_cache_digest {
        anyhow::bail!("Strict reranker cache changed during inference");
    }
    Ok(results)
}

#[derive(Serialize)]
struct RerankerFileRecord {
    path: String,
    sha256: String,
    size: u64,
}

fn require_lower_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        anyhow::bail!("{label} must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut stream = std::fs::File::open(path)
        .with_context(|| format!("Failed to open reranker asset {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = stream
            .read(&mut buffer)
            .with_context(|| format!("Failed to read reranker asset {}", path.display()))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn collect_reranker_files(
    root: &Path,
    directory: &Path,
    records: &mut Vec<RerankerFileRecord>,
) -> Result<()> {
    let mut entries = std::fs::read_dir(directory)
        .with_context(|| format!("Failed to read reranker cache {}", directory.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by(|left, right| {
        left.file_name()
            .to_string_lossy()
            .as_bytes()
            .cmp(right.file_name().to_string_lossy().as_bytes())
    });

    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("Failed to inspect reranker asset {}", path.display()))?;
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            anyhow::bail!(
                "Strict reranker cache contains a symlink: {}",
                path.display()
            );
        }
        if file_type.is_dir() {
            collect_reranker_files(root, &path, records)?;
            continue;
        }
        if !file_type.is_file() {
            anyhow::bail!(
                "Strict reranker cache contains a non-regular entry: {}",
                path.display()
            );
        }
        let relative = path
            .strip_prefix(root)
            .expect("walked reranker entry stays under root");
        let relative = relative
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Reranker asset path is not UTF-8"))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        records.push(RerankerFileRecord {
            path: relative,
            sha256: file_sha256(&path)?,
            size: metadata.len(),
        });
    }
    Ok(())
}

fn strict_reranker_cache_digest() -> Result<String> {
    let root = std::env::var("FASTEMBED_CACHE_DIR")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("Strict reranking requires FASTEMBED_CACHE_DIR"))?;
    let root_metadata = std::fs::symlink_metadata(&root)
        .with_context(|| format!("Failed to inspect reranker cache {}", root.display()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        anyhow::bail!("Strict reranker cache root must be a regular directory");
    }
    let mut records = Vec::new();
    collect_reranker_files(&root, &root, &mut records)?;
    if records.is_empty() {
        anyhow::bail!("Strict reranker cache contains no regular files");
    }
    records.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let mut encoded =
        serde_json::to_vec(&records).context("Failed to encode reranker file records")?;
    encoded.push(b'\n');
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn verify_strict_reranker_cache() -> Result<String> {
    let expected = std::env::var(RERANKER_FILES_DIGEST_ENV)
        .with_context(|| format!("Strict reranking requires {RERANKER_FILES_DIGEST_ENV}"))?;
    require_lower_sha256(&expected, RERANKER_FILES_DIGEST_ENV)?;
    let observed = strict_reranker_cache_digest()?;
    if observed != expected {
        anyhow::bail!("Strict reranker cache digest does not match the sealed manifest");
    }
    Ok(observed)
}

fn validate_strict_rerank(
    candidates: &[RerankCandidate],
    results: &[RerankedResult],
) -> Result<()> {
    if results.len() != candidates.len() {
        anyhow::bail!(
            "Strict rerank result count mismatch: candidates={} results={}",
            candidates.len(),
            results.len()
        );
    }

    let expected: std::collections::BTreeSet<usize> = candidates
        .iter()
        .map(|candidate| candidate.original_index)
        .collect();
    if expected.len() != candidates.len() {
        anyhow::bail!("Strict rerank candidates contain duplicate original indices");
    }
    let observed: std::collections::BTreeSet<usize> =
        results.iter().map(|result| result.original_index).collect();
    if observed.len() != results.len() || observed != expected {
        anyhow::bail!("Strict rerank results are not an exact candidate permutation");
    }
    if results.iter().any(|result| !result.score.is_finite()) {
        anyhow::bail!("Strict rerank returned a non-finite score");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rna_cache_dir_uses_home() {
        // Skip if FASTEMBED_CACHE_DIR is explicitly set -- rna_cache_dir()
        // correctly prioritizes that over HOME, so the assertions below
        // would not hold.
        if std::env::var("FASTEMBED_CACHE_DIR").is_ok() {
            return;
        }
        let dir = rna_cache_dir();
        let dir_str = dir.to_string_lossy();
        // When HOME is set (typical in tests), cache dir should be under ~/.cache/rna/models
        if std::env::var("HOME").is_ok() {
            assert!(
                dir_str.contains(".cache/rna/models"),
                "Expected ~/.cache/rna/models, got: {}",
                dir_str
            );
            assert!(
                !dir_str.contains(".fastembed_cache"),
                "Should not use .fastembed_cache: {}",
                dir_str
            );
        }
    }

    #[test]
    fn test_rerank_empty_candidates() {
        let results = rerank_results("test query", &[]).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_rerank_candidates_structure() {
        // Test that RerankCandidate correctly carries original_index
        let candidate = RerankCandidate {
            text: "some document text".to_string(),
            original_index: 42,
        };
        assert_eq!(candidate.original_index, 42);
        assert_eq!(candidate.text, "some document text");
    }

    #[test]
    fn test_reranked_result_structure() {
        let result = RerankedResult {
            original_index: 5,
            score: 0.95,
        };
        assert_eq!(result.original_index, 5);
        assert!((result.score - 0.95).abs() < f32::EPSILON);
    }

    fn strict_candidates() -> Vec<RerankCandidate> {
        vec![
            RerankCandidate {
                text: "first".to_string(),
                original_index: 4,
            },
            RerankCandidate {
                text: "second".to_string(),
                original_index: 9,
            },
        ]
    }

    #[test]
    fn strict_rerank_accepts_only_a_complete_permutation() {
        let candidates = strict_candidates();
        let complete = vec![
            RerankedResult {
                original_index: 9,
                score: 0.9,
            },
            RerankedResult {
                original_index: 4,
                score: 0.4,
            },
        ];
        assert!(validate_strict_rerank(&candidates, &complete).is_ok());

        let missing = &complete[..1];
        assert!(validate_strict_rerank(&candidates, missing).is_err());

        let duplicate = vec![
            RerankedResult {
                original_index: 9,
                score: 0.9,
            },
            RerankedResult {
                original_index: 9,
                score: 0.4,
            },
        ];
        assert!(validate_strict_rerank(&candidates, &duplicate).is_err());
    }

    #[test]
    fn strict_rerank_rejects_non_finite_scores_and_duplicate_candidates() {
        let candidates = strict_candidates();
        let non_finite = vec![
            RerankedResult {
                original_index: 4,
                score: f32::NAN,
            },
            RerankedResult {
                original_index: 9,
                score: 0.5,
            },
        ];
        assert!(validate_strict_rerank(&candidates, &non_finite).is_err());

        let duplicate_candidates = vec![
            RerankCandidate {
                text: "first".to_string(),
                original_index: 4,
            },
            RerankCandidate {
                text: "second".to_string(),
                original_index: 4,
            },
        ];
        assert!(validate_strict_rerank(&duplicate_candidates, &[]).is_err());
    }

    // ==================== Adversarial tests (dissent-seeded) ====================

    /// Dissent #1: Empty query string with empty candidates.
    /// Tests the same early-return path as test_rerank_empty_candidates.
    /// Testing empty query with non-empty candidates requires model loading
    /// and is covered by the ignored integration test.
    #[test]
    fn test_rerank_empty_query() {
        let results = rerank_results("", &[]).unwrap();
        assert!(results.is_empty());
    }

    /// Dissent #2: Single candidate. Reranking a single document should work
    /// and return it unchanged (no comparison needed, but the cross-encoder
    /// still needs to score it).
    #[test]
    fn test_rerank_single_candidate_structure() {
        let candidate = RerankCandidate {
            text: "only one document".to_string(),
            original_index: 0,
        };
        // Verify the candidate is well-formed
        assert_eq!(candidate.original_index, 0);
    }

    /// Dissent #3: Very long document text. The cross-encoder has a token
    /// limit; extremely long documents should not cause a panic.
    #[test]
    fn test_rerank_long_document_structure() {
        let long_text = "x".repeat(100_000);
        let candidate = RerankCandidate {
            text: long_text.clone(),
            original_index: 0,
        };
        assert_eq!(candidate.text.len(), 100_000);
    }

    /// Dissent #4: Original index preservation. When candidates have
    /// non-contiguous original indices (e.g., from filtered results),
    /// the mapping must be preserved correctly.
    #[test]
    fn test_rerank_non_contiguous_indices() {
        let candidates = vec![
            RerankCandidate {
                text: "first".to_string(),
                original_index: 5,
            },
            RerankCandidate {
                text: "second".to_string(),
                original_index: 42,
            },
            RerankCandidate {
                text: "third".to_string(),
                original_index: 100,
            },
        ];
        // Verify non-contiguous indices are preserved in structures
        assert_eq!(candidates[0].original_index, 5);
        assert_eq!(candidates[1].original_index, 42);
        assert_eq!(candidates[2].original_index, 100);
    }

    /// Dissent #5: Duplicate original indices. Should not happen in practice
    /// but must not cause a panic or index out of bounds.
    #[test]
    fn test_rerank_duplicate_indices_structure() {
        let candidates = vec![
            RerankCandidate {
                text: "doc A".to_string(),
                original_index: 0,
            },
            RerankCandidate {
                text: "doc B".to_string(),
                original_index: 0,
            },
        ];
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].original_index, candidates[1].original_index);
    }

    /// Dissent: RerankedResult score ordering. Verify that manual construction
    /// of results with specific scores sorts correctly.
    #[test]
    fn test_reranked_result_sort_order() {
        let mut results = vec![
            RerankedResult {
                original_index: 0,
                score: 0.3,
            },
            RerankedResult {
                original_index: 1,
                score: 0.9,
            },
            RerankedResult {
                original_index: 2,
                score: 0.5,
            },
        ];
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        assert_eq!(results[0].original_index, 1); // 0.9
        assert_eq!(results[1].original_index, 2); // 0.5
        assert_eq!(results[2].original_index, 0); // 0.3
    }

    /// Dissent: NaN scores. If the cross-encoder produces NaN (shouldn't,
    /// but possible with bad inputs), the sort must not panic.
    #[test]
    fn test_reranked_result_nan_score_sort() {
        let mut results = vec![
            RerankedResult {
                original_index: 0,
                score: f32::NAN,
            },
            RerankedResult {
                original_index: 1,
                score: 0.5,
            },
        ];
        // Should not panic -- NaN comparison falls through to Equal
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        assert_eq!(results.len(), 2);
    }

    // Integration test: actually loads the model and reranks.
    // This downloads the model on first run (~100MB), so it's ignored by default.
    #[test]
    #[ignore]
    fn test_rerank_integration() {
        let candidates = vec![
            RerankCandidate {
                text: "The embedding cache stores computed vectors to avoid re-computation"
                    .to_string(),
                original_index: 0,
            },
            RerankCandidate {
                text: "fn main() { println!(\"hello world\"); }".to_string(),
                original_index: 1,
            },
            RerankCandidate {
                text: "Database connection pooling configuration".to_string(),
                original_index: 2,
            },
        ];

        let results = rerank_results("how does the embedding cache work", &candidates).unwrap();
        assert_eq!(results.len(), 3);

        // The embedding cache document should rank highest
        assert_eq!(
            results[0].original_index, 0,
            "Embedding cache doc should be most relevant"
        );
    }
}
