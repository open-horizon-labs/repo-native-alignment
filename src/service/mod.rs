//! Shared service layer for CLI and MCP.
//!
//! Both interfaces are thin dispatchers to these functions. The service layer
//! defines the full capability surface -- adding a parameter here automatically
//! makes it available in both CLI and MCP.

use std::collections::HashSet;
use std::path::Path;

use crate::embed::EmbeddingIndex;
use crate::server::{EmbeddingStatus, EnrichmentJobRecord, GraphState, LspEnrichmentStatus};

pub mod graph;
pub mod progress;
pub mod repomap;
pub mod roots;
pub mod search;

// Re-export all public items so callers keep working without path changes.
pub use graph::{GraphParams, StatsResult, graph_query, stats};
pub use progress::{OutcomeProgressContext, OutcomeProgressParams, outcome_progress};
pub use repomap::{RepoMapContext, RepoMapParams, repo_map};
pub use roots::{list_roots, list_roots_from_slugs};
pub use search::search;

/// Interface-agnostic search parameters.
///
/// The manual `Default` impl intentionally sets `include_artifacts` and
/// `include_markdown` to `true`, matching the MCP tool's `default_true()`
/// defaults. `derive(Default)` would leave them `false`, diverging from
/// `from_mcp_search()` and silently disabling artifact/markdown search for
/// any code that constructs `SearchParams::default()`.
#[derive(Debug)]
pub struct SearchParams {
    pub query: Option<String>,
    pub node: Option<String>,
    pub mode: Option<String>,
    pub hops: Option<u32>,
    pub depth: Option<u32>,
    pub direction: Option<String>,
    pub edge_types: Option<Vec<String>>,
    pub kind: Option<String>,
    pub language: Option<String>,
    pub file: Option<String>,
    pub limit: Option<usize>,
    pub sort_by: Option<String>,
    pub min_complexity: Option<u32>,
    pub synthetic: Option<bool>,
    pub compact: bool,
    pub nodes: Option<Vec<String>>,
    pub search_mode: Option<String>,
    pub rerank: bool,
    pub include_artifacts: bool,
    pub include_markdown: bool,
    pub artifact_types: Option<Vec<String>>,
    pub subsystem: Option<String>,
    pub target_subsystem: Option<String>,
    pub include_body: bool,
    pub minify_body: bool,
    pub verbose: bool,
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            query: None,
            node: None,
            mode: None,
            hops: None,
            depth: None,
            direction: None,
            edge_types: None,
            kind: None,
            language: None,
            file: None,
            limit: None,
            sort_by: None,
            min_complexity: None,
            synthetic: None,
            compact: false,
            nodes: None,
            search_mode: None,
            rerank: false,
            // Default to true to match MCP tool defaults (`default_true()` on the
            // `Search` struct). `derive(Default)` would leave these `false`, causing
            // `SearchParams::default()` callers to silently get code-only search.
            include_artifacts: true,
            include_markdown: true,
            artifact_types: None,
            subsystem: None,
            target_subsystem: None,
            include_body: false,
            minify_body: false,
            verbose: false,
        }
    }
}

fn non_blank_optional(value: &Option<String>) -> Option<String> {
    value.as_deref().and_then(|s| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn non_empty_string_vec(value: &Option<Vec<String>>) -> Option<Vec<String>> {
    let values: Vec<String> = value
        .as_ref()?
        .iter()
        .filter_map(|s| {
            let trimmed = s.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .collect();

    (!values.is_empty()).then_some(values)
}

impl SearchParams {
    pub fn normalized_mode(&self) -> Option<&str> {
        self.mode
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// Convert from MCP `Search` tool struct.
    pub fn from_mcp_search(args: &crate::server::tools::Search) -> Self {
        Self {
            query: non_blank_optional(&args.query),
            node: non_blank_optional(&args.node),
            mode: non_blank_optional(&args.mode),
            hops: args.hops,
            depth: args.depth,
            direction: non_blank_optional(&args.direction),
            edge_types: non_empty_string_vec(&args.edge_types),
            kind: non_blank_optional(&args.kind),
            language: non_blank_optional(&args.language),
            file: non_blank_optional(&args.file),
            limit: args.limit.map(|k| k as usize),
            sort_by: non_blank_optional(&args.sort_by),
            min_complexity: args.min_complexity,
            synthetic: args.synthetic,
            compact: args.compact.unwrap_or(false),
            nodes: non_empty_string_vec(&args.nodes),
            search_mode: non_blank_optional(&args.search_mode),
            rerank: args.rerank.unwrap_or(true),
            include_artifacts: args.include_artifacts.unwrap_or(true),
            include_markdown: args.include_markdown.unwrap_or(true),
            artifact_types: non_empty_string_vec(&args.artifact_types),
            subsystem: non_blank_optional(&args.subsystem),
            target_subsystem: non_blank_optional(&args.target_subsystem),
            include_body: args.include_body.unwrap_or(false),
            minify_body: args.minify_body.unwrap_or(false),
            verbose: args.verbose.unwrap_or(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::tools::Search;
    use serde_json::json;

    #[test]
    fn from_mcp_search_treats_blank_mode_as_absent() {
        let search: Search = serde_json::from_value(json!({
            "query": "parse_search",
            "mode": ""
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.query.as_deref(), Some("parse_search"));
        assert!(params.mode.is_none());
    }

    #[test]
    fn from_mcp_search_treats_whitespace_only_mode_as_absent() {
        let search: Search = serde_json::from_value(json!({
            "query": "parse_search",
            "mode": "   "
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.query.as_deref(), Some("parse_search"));
        assert!(params.mode.is_none());
    }

    #[test]
    fn from_mcp_search_trims_mode() {
        let search: Search = serde_json::from_value(json!({
            "node": "repo:src/lib.rs:thing:function",
            "mode": " neighbors "
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.mode.as_deref(), Some("neighbors"));
    }

    #[test]
    fn from_mcp_search_preserves_invalid_non_blank_mode() {
        let search: Search = serde_json::from_value(json!({
            "node": "repo:src/lib.rs:thing:function",
            "mode": " bogus "
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.mode.as_deref(), Some("bogus"));
    }

    #[test]
    fn from_mcp_search_treats_empty_nodes_as_absent() {
        let search: Search = serde_json::from_value(json!({
            "query": "get_graph",
            "nodes": [],
            "edge_types": [],
            "artifact_types": []
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.query.as_deref(), Some("get_graph"));
        assert!(params.nodes.is_none());
        assert!(params.edge_types.is_none());
        assert!(params.artifact_types.is_none());
    }

    #[test]
    fn from_mcp_search_trims_and_drops_blank_nodes() {
        let search: Search = serde_json::from_value(json!({
            "nodes": ["  repo:src/lib.rs:thing:function  ", "", "   "]
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(
            params.nodes,
            Some(vec!["repo:src/lib.rs:thing:function".to_string()])
        );
    }

    #[test]
    fn from_mcp_search_treats_blank_scalar_filters_as_absent() {
        let search: Search = serde_json::from_value(json!({
            "query": "  ",
            "node": "",
            "kind": "   ",
            "language": "",
            "file": " ",
            "direction": "   ",
            "sort_by": "",
            "search_mode": " ",
            "subsystem": "",
            "target_subsystem": "  "
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert!(params.query.is_none());
        assert!(params.node.is_none());
        assert!(params.kind.is_none());
        assert!(params.language.is_none());
        assert!(params.file.is_none());
        assert!(params.direction.is_none());
        assert!(params.sort_by.is_none());
        assert!(params.search_mode.is_none());
        assert!(params.subsystem.is_none());
        assert!(params.target_subsystem.is_none());
    }

    #[test]
    fn from_mcp_search_trims_vector_filters() {
        let search: Search = serde_json::from_value(json!({
            "edge_types": [" calls ", "", "   "],
            "artifact_types": [" outcome ", ""]
        }))
        .unwrap();

        let params = SearchParams::from_mcp_search(&search);

        assert_eq!(params.edge_types, Some(vec!["calls".to_string()]));
        assert_eq!(params.artifact_types, Some(vec!["outcome".to_string()]));
    }
}

/// Runtime context for search operations.
pub struct SearchContext<'a> {
    pub graph_state: &'a GraphState,
    pub embed_index: Option<&'a EmbeddingIndex>,
    pub repo_root: &'a Path,
    pub lsp_status: Option<&'a LspEnrichmentStatus>,
    pub embed_status: Option<&'a EmbeddingStatus>,
    pub root_filter: Option<String>,
    pub non_code_slugs: HashSet<String>,
    pub enrichment_jobs: Vec<EnrichmentJobRecord>,
}

/// Returns true when a graph node's root passes the active root filter.
///
/// `None` filter matches any root. When a filter is set, the node must match
/// the slug (case-insensitive), OR be the synthetic "external" root (so that
/// external dependencies always appear in traversal results), OR be a
/// non-code slug (e.g., a memory root that stores markdown rather than code).
pub fn node_passes_root_filter(
    node_root: &str,
    root_filter: &Option<String>,
    non_code_slugs: &HashSet<String>,
) -> bool {
    match root_filter {
        None => true,
        Some(slug) => {
            node_root.eq_ignore_ascii_case(slug)
                || node_root == "external"
                || non_code_slugs.contains(node_root)
        }
    }
}

/// Returns true when an embedding search result passes the active root filter.
///
/// Non-code results (artifacts, markdown) always pass — they are not root-
/// scoped the same way code symbols are. Code results delegate to
/// `node_passes_root_filter` using the root prefix of the result ID.
pub fn search_result_passes_root_filter(
    result: &crate::embed::SearchResult,
    root_filter: &Option<String>,
    non_code_slugs: &HashSet<String>,
) -> bool {
    if root_filter.is_none() {
        return true;
    }
    if !result.kind.starts_with("code:") {
        return true;
    }
    node_passes_root_filter(
        result.id.split(':').next().unwrap_or(""),
        root_filter,
        non_code_slugs,
    )
}
