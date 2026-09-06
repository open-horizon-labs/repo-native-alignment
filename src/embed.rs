#[cfg(feature = "embeddings")]
#[path = "embed/real.rs"]
mod real;

#[cfg(feature = "embeddings")]
pub mod config;

#[cfg(feature = "cuda")]
mod cuda_encoder;
#[path = "embed/generation.rs"]
pub mod generation;

#[cfg(feature = "embeddings")]
pub use real::*;

pub const EMBEDDING_MODEL_NAME: &str = "sentence-transformers/all-MiniLM-L6-v2";

#[cfg(not(feature = "embeddings"))]
pub fn require_metal_device() -> anyhow::Result<generation::DeviceAttestation> {
    anyhow::bail!(
        "strict embedding execution requires an artifact built with embeddings and Metal support"
    )
}

#[cfg(not(feature = "embeddings"))]
use std::path::Path;

#[cfg(not(feature = "embeddings"))]
use anyhow::{Result, anyhow};

#[cfg(not(feature = "embeddings"))]
#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    /// Filter to symbols in this subsystem (exact match on `subsystem` column).
    pub subsystem: Option<String>,
    /// Filter to symbols in files whose path contains this substring
    /// (LIKE '%...%' on `file_path` column).
    pub file: Option<String>,
    /// Filter to symbols in this language (exact match on `language` column).
    pub language: Option<String>,
    /// Filter to symbols with cyclomatic complexity >= this value.
    pub min_complexity: Option<u32>,
}

#[cfg(not(feature = "embeddings"))]
impl SearchFilters {
    /// Build a LanceDB SQL filter expression from the active filters.
    /// Returns `None` if no filters are set.
    pub fn to_sql(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();

        if let Some(ref sub) = self.subsystem {
            let escaped = sub.replace('\'', "''");
            parts.push(format!("subsystem = '{}'", escaped));
        }

        if let Some(ref file) = self.file {
            let escaped = file
                .replace('\'', "''")
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_");
            parts.push(format!("file_path LIKE '%{}%' ESCAPE '!'", escaped));
        }

        if let Some(ref lang) = self.language {
            let escaped = lang.replace('\'', "''");
            parts.push(format!("language = '{}'", escaped));
        }

        if let Some(min_cc) = self.min_complexity {
            parts.push(format!("cyclomatic >= {}", min_cc));
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" AND "))
        }
    }
}

#[cfg(not(feature = "embeddings"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// Combine keyword (BM25) + vector scoring when embeddings are enabled.
    #[default]
    Hybrid,
    /// Pure keyword search when embeddings are enabled.
    Keyword,
    /// Pure vector search when embeddings are enabled.
    Semantic,
}

/// Retrieval channel that actually executed after any allowed fallback.
#[cfg(not(feature = "embeddings"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutedSearchMode {
    Keyword,
    Semantic,
    HybridRrf,
}

/// Explicit product policy for test-path results.
#[cfg(not(feature = "embeddings"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestResultPolicy {
    Demote,
    Neutral,
}

/// Native score emitted by the retrieval backend before product transforms.
#[cfg(not(feature = "embeddings"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeScoreKind {
    Bm25,
    CosineDistance,
    HybridRrfRelevance,
}

/// Whether a native value came from the backend or a deterministic fallback.
#[cfg(not(feature = "embeddings"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeScoreSource {
    Backend,
    DeterministicFallback,
}

/// Deterministic normalization applied before product ranking policy.
#[cfg(not(feature = "embeddings"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreNormalization {
    NonNegativeSaturation,
    OneMinusDistanceFloorZero,
}

/// Product adjustment applied after score normalization.
#[cfg(not(feature = "embeddings"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreAdjustment {
    None,
    TestPathDemotion70Percent,
}

#[cfg(not(feature = "embeddings"))]
impl ScoreAdjustment {
    pub const fn factor(self) -> f32 {
        match self {
            Self::None => 1.0,
            Self::TestPathDemotion70Percent => 0.7,
        }
    }
}

/// Score audit record aligned with one returned [`SearchResult`].
#[cfg(not(feature = "embeddings"))]
#[derive(Debug, Clone, PartialEq)]
pub struct SearchScoreProvenance {
    pub result_id: String,
    pub native_kind: NativeScoreKind,
    pub native_value: f32,
    pub native_source: NativeScoreSource,
    pub normalization: ScoreNormalization,
    pub normalized_score: f32,
    pub adjustment: ScoreAdjustment,
}

#[cfg(not(feature = "embeddings"))]
impl SearchScoreProvenance {
    pub fn product_score(&self) -> f32 {
        self.normalized_score * self.adjustment.factor()
    }
}

#[cfg(not(feature = "embeddings"))]
pub enum SearchOutcome {
    /// Index is ready; here are the results (may be empty).
    Results(Vec<SearchResult>),
    /// Embedding support is not compiled in or the table is not ready.
    NotReady,
}

/// Search result plus truthful execution metadata for calibrated product fusion.
#[cfg(not(feature = "embeddings"))]
pub struct ObservedSearchOutcome {
    pub outcome: SearchOutcome,
    pub executed_mode: Option<ExecutedSearchMode>,
    /// Same order as `SearchOutcome::Results`; empty for `NotReady`.
    pub score_provenance: Vec<SearchScoreProvenance>,
}

#[cfg(not(feature = "embeddings"))]
pub struct SearchResult {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub score: f32,
}

#[cfg(not(feature = "embeddings"))]
impl SearchResult {
    pub fn to_markdown(&self) -> String {
        let snippet = if self.body.chars().count() > 200 {
            format!("{}...", truncate_chars(&self.body, 200))
        } else {
            self.body.clone()
        };
        if self.kind.starts_with("code:") {
            format!(
                "- **{}** ({}) — relevance: {:.2}\n  {}\n  ID: `{}`\n",
                self.title, self.kind, self.score, snippet, self.id
            )
        } else if self.kind == "commit" {
            format!(
                "- **{}** ({}) — relevance: {:.2}\n  {}\n  Hash: `{}` (use: `git show {}`)\n",
                self.title, self.kind, self.score, snippet, self.id, self.id
            )
        } else {
            format!(
                "- **{}** ({}) — relevance: {:.2}\n  {}\n",
                self.title, self.kind, self.score, snippet
            )
        }
    }
}

#[cfg(not(feature = "embeddings"))]
#[derive(Clone, Debug, Default)]
pub struct EmbeddingIndex;

#[cfg(not(feature = "embeddings"))]
impl EmbeddingIndex {
    pub fn runtime_diagnostic(&self) -> String {
        "requested_backend=unavailable effective_backend=unavailable (built without embeddings feature)".to_string()
    }

    pub async fn new(_repo_root: &Path) -> Result<Self> {
        Ok(Self)
    }

    pub async fn open_existing(_repo_root: &Path) -> Result<Option<Self>> {
        Ok(None)
    }

    pub async fn open_existing_offline(_repo_root: &Path) -> Result<Option<Self>> {
        Ok(None)
    }

    pub fn resident_query_runtime(&self) -> bool {
        false
    }

    pub async fn new_strict(_repo_root: &Path) -> Result<Self> {
        Err(anyhow!(
            "strict embedding execution requires an artifact built with embeddings and Metal support"
        ))
    }

    pub(crate) async fn new_for_reconciliation(_repo_root: &Path) -> Result<Self> {
        Ok(Self)
    }

    pub async fn has_table(&self) -> Result<bool> {
        Ok(false)
    }

    pub fn active_generation_manifest(&self) -> Option<generation::GenerationManifest> {
        None
    }

    pub fn verified_generation_evidence(
        &self,
    ) -> Result<
        Option<(
            generation::GenerationManifest,
            generation::SemanticVerificationReceipt,
        )>,
    > {
        Ok(None)
    }

    pub async fn verified_generation_evidence_for_persisted_graph(
        &self,
        _nodes: &[crate::graph::Node],
        _edges: &[crate::graph::Edge],
        _business_context: &crate::business_context::BusinessContextAdmission,
    ) -> Result<
        Option<(
            generation::GenerationManifest,
            generation::SemanticVerificationReceipt,
        )>,
    > {
        Ok(None)
    }

    pub async fn ensure_fts_index(&self) {}

    pub async fn index_all_with_symbols(
        &self,
        _repo_root: &Path,
        _symbols: &[crate::graph::Node],
    ) -> Result<usize> {
        Err(anyhow!(
            "embeddings support is not compiled in; rebuild with --features embeddings"
        ))
    }

    pub async fn index_all_with_symbols_and_business_context(
        &self,
        _repo_root: &Path,
        _symbols: &[crate::graph::Node],
        _business_context: &crate::business_context::BusinessContextAdmission,
    ) -> Result<usize> {
        Err(anyhow!(
            "embeddings support is not compiled in; rebuild with --features embeddings"
        ))
    }

    pub async fn index_all_with_persisted_graph_and_business_context(
        &self,
        _repo_root: &Path,
        _symbols: &[crate::graph::Node],
        _edges: &[crate::graph::Edge],
        _business_context: &crate::business_context::BusinessContextAdmission,
    ) -> Result<usize> {
        Err(anyhow!(
            "embeddings support is not compiled in; rebuild with --features embeddings"
        ))
    }

    pub async fn index_all(&self, _repo_root: &Path) -> Result<usize> {
        Err(anyhow!(
            "embeddings support is not compiled in; rebuild with --features embeddings"
        ))
    }

    pub async fn reindex_nodes(&self, _nodes: &[crate::graph::Node]) -> Result<usize> {
        Err(anyhow!(
            "embeddings support is not compiled in; rebuild with --features embeddings"
        ))
    }

    pub async fn search(
        &self,
        _query: &str,
        _artifact_types: Option<&[String]>,
        _limit: usize,
    ) -> Result<SearchOutcome> {
        Ok(SearchOutcome::NotReady)
    }

    pub async fn search_with_mode(
        &self,
        _query: &str,
        _artifact_types: Option<&[String]>,
        _limit: usize,
        _mode: SearchMode,
    ) -> Result<SearchOutcome> {
        Ok(SearchOutcome::NotReady)
    }

    pub async fn search_with_filters(
        &self,
        _query: &str,
        _artifact_types: Option<&[String]>,
        _limit: usize,
        _mode: SearchMode,
        _filters: &SearchFilters,
    ) -> Result<SearchOutcome> {
        Ok(SearchOutcome::NotReady)
    }

    pub async fn search_with_filters_observed(
        &self,
        _query: &str,
        _artifact_types: Option<&[String]>,
        _limit: usize,
        _mode: SearchMode,
        _filters: &SearchFilters,
        _test_policy: TestResultPolicy,
    ) -> Result<ObservedSearchOutcome> {
        Ok(ObservedSearchOutcome {
            outcome: SearchOutcome::NotReady,
            executed_mode: None,
            score_provenance: Vec::new(),
        })
    }

    pub async fn search_with_filters_strict(
        &self,
        _query: &str,
        _artifact_types: Option<&[String]>,
        _limit: usize,
        _mode: SearchMode,
        _filters: &SearchFilters,
    ) -> Result<SearchOutcome> {
        Err(anyhow!(
            "strict semantic search requires an artifact built with embeddings and Metal support"
        ))
    }
}

#[cfg(not(feature = "embeddings"))]
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

#[cfg(all(test, not(feature = "embeddings")))]
mod tests {
    use super::SearchResult;

    #[test]
    fn search_result_markdown_truncates_on_char_boundary() {
        let result = SearchResult {
            id: "abc123".into(),
            kind: "commit".into(),
            title: "unicode body".into(),
            body: format!("{}—tail", "a".repeat(199)),
            score: 1.0,
        };

        let markdown = result.to_markdown();

        assert!(markdown.contains("..."));
        assert!(markdown.contains(&format!("{}—...", "a".repeat(199))));
        assert!(markdown.contains("Hash: `abc123`"));
    }
}
