#[cfg(feature = "embeddings")]
#[path = "embed/real.rs"]
mod real;

#[cfg(feature = "embeddings")]
pub use real::*;

pub const EMBEDDING_MODEL_NAME: &str = "MiniLM-L6-v2";
pub const EMBEDDING_MODEL_REPOSITORY: &str = "sentence-transformers/all-MiniLM-L6-v2";

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    /// Combine keyword (BM25) + vector scoring when embeddings are enabled.
    #[default]
    Hybrid,
    /// Pure keyword search when embeddings are enabled.
    Keyword,
    /// Pure vector search when embeddings are enabled.
    Semantic,
}

#[cfg(not(feature = "embeddings"))]
pub enum SearchOutcome {
    /// Index is ready; here are the results (may be empty).
    Results(Vec<SearchResult>),
    /// Embedding support is not compiled in or the table is not ready.
    NotReady,
}

#[cfg(not(feature = "embeddings"))]
#[derive(Debug, Clone, serde::Serialize)]
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
    pub async fn new(_repo_root: &Path) -> Result<Self> {
        Ok(Self)
    }

    pub async fn has_table(&self) -> Result<bool> {
        Ok(false)
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

    pub async fn search_with_filters_strict(
        &self,
        _query: &str,
        _artifact_types: Option<&[String]>,
        _limit: usize,
        _mode: SearchMode,
        _filters: &SearchFilters,
    ) -> Result<SearchOutcome> {
        Err(anyhow!(
            "strict semantic search requires an embeddings-enabled build"
        ))
    }
}

#[cfg(not(feature = "embeddings"))]
pub fn runtime_acceleration() -> Result<&'static str> {
    Err(anyhow!("embeddings support is not compiled in"))
}

#[cfg(not(feature = "embeddings"))]
pub async fn probe_embedding_runtime() -> Result<()> {
    Err(anyhow!("embeddings support is not compiled in"))
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
