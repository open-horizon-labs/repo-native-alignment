//! Cross-encoder reranking for search results.
//!
//! After initial retrieval (BM25, vector, or hybrid RRF), the top-N candidates
//! can be re-scored by a cross-encoder model that attends to query-document
//! pairs jointly. This produces more precise relevance scores than bi-encoder
//! embeddings, which encode query and document independently.
//!
//! The reranker is opt-in (`rerank: true` on the search tool) to avoid latency
//! regression for simple lookups. It is lazy-loaded on first use.

use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use fastembed::{RerankerModel, RerankResult, TextRerank};

/// Global lazy-loaded reranker instance behind a Mutex.
/// The `TextRerank::rerank()` method requires `&mut self` (ONNX session state),
/// so we wrap in `Mutex` for safe concurrent access. `OnceLock` ensures the
/// model is loaded exactly once.
static RERANKER: OnceLock<Mutex<Result<TextRerank, String>>> = OnceLock::new();

/// Default model: Jina Reranker V1 Turbo (English).
/// Smaller and faster than BGE-Reranker-Base while maintaining good quality.
const DEFAULT_MODEL: RerankerModel = RerankerModel::JINARerankerV1TurboEn;

/// Initialize the reranker on first call, then return a locked reference.
fn init_reranker() -> &'static Mutex<Result<TextRerank, String>> {
    RERANKER.get_or_init(|| {
        let start = std::time::Instant::now();
        tracing::info!("Reranker: loading {:?} (first use, one-time cost)", DEFAULT_MODEL);

        let reranker = TextRerank::try_new(fastembed::RerankInitOptions::new(DEFAULT_MODEL))
            .map_err(|e| format!("Failed to load reranker model: {}", e));

        match &reranker {
            Ok(_) => tracing::info!("Reranker: ready in {:?}", start.elapsed()),
            Err(e) => tracing::warn!("Reranker: load failed in {:?}: {}", start.elapsed(), e),
        }

        Mutex::new(reranker)
    })
}

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
pub fn rerank_results(
    query: &str,
    candidates: &[RerankCandidate],
) -> Result<Vec<RerankedResult>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let mutex = init_reranker();
    let mut guard = mutex.lock().map_err(|e| anyhow::anyhow!("Reranker lock poisoned: {}", e))?;
    let reranker = guard.as_mut().map_err(|e| anyhow::anyhow!("{}", e))?;

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
    let mut results: Vec<RerankedResult> = rerank_results
        .into_iter()
        .map(|r| RerankedResult {
            original_index: candidates[r.index].original_index,
            score: r.score as f32,
        })
        .collect();

    // Sort by score descending (fastembed may already do this, but be explicit).
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // Integration test: actually loads the model and reranks.
    // This downloads the model on first run (~100MB), so it's ignored by default.
    #[test]
    #[ignore]
    fn test_rerank_integration() {
        let candidates = vec![
            RerankCandidate {
                text: "The embedding cache stores computed vectors to avoid re-computation".to_string(),
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
