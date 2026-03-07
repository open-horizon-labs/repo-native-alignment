use std::path::Path;

use anyhow::Result;

use crate::code;
use crate::git;
use crate::markdown;
use crate::oh;
use crate::types::{OhArtifact, QueryResult};

/// The main intersection query: searches across all layers (.oh/ artifacts,
/// markdown chunks, code symbols, git commits) using case-insensitive
/// substring matching.
pub fn query_all(repo_root: &Path, query: &str) -> Result<QueryResult> {
    // Load and search .oh/ artifacts
    let all_artifacts = oh::load_oh_artifacts(repo_root).unwrap_or_else(|e| {
        tracing::warn!("Failed to load .oh/ artifacts: {}", e);
        Vec::new()
    });
    let outcomes = search_oh_artifacts(&all_artifacts, query);

    // Load and search markdown chunks
    let all_chunks = markdown::extract_markdown_chunks(repo_root).unwrap_or_else(|e| {
        tracing::warn!("Failed to extract markdown chunks: {}", e);
        Vec::new()
    });
    let markdown_chunks = markdown::search_chunks(&all_chunks, query)
        .into_iter()
        .cloned()
        .collect();

    // Load and search code symbols
    let all_symbols = code::extract_symbols(repo_root).unwrap_or_else(|e| {
        tracing::warn!("Failed to extract code symbols: {}", e);
        Vec::new()
    });
    let code_symbols = code::search_symbols(&all_symbols, query)
        .into_iter()
        .cloned()
        .collect();

    // Search git commits by message
    let commits = git::search_commits(repo_root, query, 50).unwrap_or_else(|e| {
        tracing::warn!("Failed to search git commits: {}", e);
        Vec::new()
    });

    Ok(QueryResult {
        query: query.to_string(),
        outcomes,
        markdown_chunks,
        code_symbols,
        commits,
    })
}

/// Returns the full, unfiltered context across all layers. Used by the
/// `oh_get_context` tool to give agents a complete picture of the repo.
pub fn get_full_context(repo_root: &Path) -> Result<QueryResult> {
    let outcomes = oh::load_oh_artifacts(repo_root).unwrap_or_else(|e| {
        tracing::warn!("Failed to load .oh/ artifacts: {}", e);
        Vec::new()
    });
    let markdown_chunks = markdown::extract_markdown_chunks(repo_root).unwrap_or_else(|e| {
        tracing::warn!("Failed to extract markdown chunks: {}", e);
        Vec::new()
    });
    let code_symbols = code::extract_symbols(repo_root).unwrap_or_else(|e| {
        tracing::warn!("Failed to extract code symbols: {}", e);
        Vec::new()
    });
    let commits = git::load_commits(repo_root, 50).unwrap_or_else(|e| {
        tracing::warn!("Failed to load git commits: {}", e);
        Vec::new()
    });

    Ok(QueryResult {
        query: String::from("(full context)"),
        outcomes,
        markdown_chunks,
        code_symbols,
        commits,
    })
}

/// Filter `.oh/` artifacts by case-insensitive substring match against the
/// artifact id, body, and all string values in the frontmatter.
fn search_oh_artifacts(artifacts: &[OhArtifact], query: &str) -> Vec<OhArtifact> {
    let query_lower = query.to_lowercase();
    artifacts
        .iter()
        .filter(|artifact| {
            // Match against id
            if artifact.id().to_lowercase().contains(&query_lower) {
                return true;
            }
            // Match against body
            if artifact.body.to_lowercase().contains(&query_lower) {
                return true;
            }
            // Match against all frontmatter string values
            artifact.frontmatter.values().any(|v| {
                yaml_value_contains(v, &query_lower)
            })
        })
        .cloned()
        .collect()
}

/// Recursively check if a YAML value contains the query string (case-insensitive).
fn yaml_value_contains(value: &serde_yaml::Value, query_lower: &str) -> bool {
    match value {
        serde_yaml::Value::String(s) => s.to_lowercase().contains(query_lower),
        serde_yaml::Value::Bool(b) => b.to_string().contains(query_lower),
        serde_yaml::Value::Number(n) => n.to_string().contains(query_lower),
        serde_yaml::Value::Sequence(seq) => {
            seq.iter().any(|v| yaml_value_contains(v, query_lower))
        }
        serde_yaml::Value::Mapping(map) => {
            map.values().any(|v| yaml_value_contains(v, query_lower))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::types::{OhArtifact, OhArtifactKind};

    fn make_artifact(id: &str, body: &str) -> OhArtifact {
        OhArtifact {
            kind: OhArtifactKind::Outcome,
            file_path: PathBuf::from(format!(".oh/outcomes/{}.md", id)),
            frontmatter: BTreeMap::from([
                (
                    "id".to_string(),
                    serde_yaml::Value::String(id.to_string()),
                ),
                (
                    "status".to_string(),
                    serde_yaml::Value::String("active".to_string()),
                ),
            ]),
            body: body.to_string(),
        }
    }

    #[test]
    fn test_search_oh_artifacts_by_id() {
        let artifacts = vec![
            make_artifact("revenue-growth", "Grow revenue by 20%"),
            make_artifact("customer-retention", "Retain customers"),
        ];
        let results = search_oh_artifacts(&artifacts, "revenue");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id(), "revenue-growth");
    }

    #[test]
    fn test_search_oh_artifacts_by_body() {
        let artifacts = vec![
            make_artifact("o1", "Improve latency to under 100ms"),
            make_artifact("o2", "Ship the new dashboard"),
        ];
        let results = search_oh_artifacts(&artifacts, "latency");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id(), "o1");
    }

    #[test]
    fn test_search_oh_artifacts_by_frontmatter_value() {
        let artifacts = vec![
            make_artifact("o1", "Body text"),
            make_artifact("o2", "Other body"),
        ];
        let results = search_oh_artifacts(&artifacts, "active");
        // Both have status: active in frontmatter
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_oh_artifacts_case_insensitive() {
        let artifacts = vec![make_artifact("Revenue-Growth", "GROW revenue")];
        let results = search_oh_artifacts(&artifacts, "REVENUE");
        assert_eq!(results.len(), 1);

        let results = search_oh_artifacts(&artifacts, "revenue-growth");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_oh_artifacts_no_match() {
        let artifacts = vec![make_artifact("o1", "Something")];
        let results = search_oh_artifacts(&artifacts, "nonexistent");
        assert!(results.is_empty());
    }

    #[test]
    fn test_yaml_value_contains_nested() {
        let seq = serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::String("alpha".to_string()),
            serde_yaml::Value::String("beta".to_string()),
        ]);
        assert!(yaml_value_contains(&seq, "alpha"));
        assert!(!yaml_value_contains(&seq, "gamma"));
    }
}
