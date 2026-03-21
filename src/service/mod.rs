//! Shared service layer for CLI and MCP.
//!
//! Both interfaces are thin dispatchers to these functions. The service layer
//! defines the full capability surface -- adding a parameter here automatically
//! makes it available in both CLI and MCP.

use std::collections::HashSet;
use std::path::Path;

use crate::embed::EmbeddingIndex;
use crate::server::state::{EmbeddingStatus, GraphState, LspEnrichmentStatus};

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
#[derive(Debug, Default)]
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
}

impl SearchParams {
    /// Convert from MCP `Search` tool struct.
    pub fn from_mcp_search(args: &crate::server::tools::Search) -> Self {
        Self {
            query: args.query.clone(),
            node: args.node.clone(),
            mode: args.mode.clone(),
            hops: args.hops,
            depth: args.depth,
            direction: args.direction.clone(),
            edge_types: args.edge_types.clone(),
            kind: args.kind.clone(),
            language: args.language.clone(),
            file: args.file.clone(),
            limit: args.top_k.map(|k| k as usize),
            sort_by: args.sort_by.clone(),
            min_complexity: args.min_complexity,
            synthetic: args.synthetic,
            compact: args.compact.unwrap_or(false),
            nodes: args.nodes.clone(),
            search_mode: args.search_mode.clone(),
            rerank: args.rerank.unwrap_or(false),
            include_artifacts: args.include_artifacts.unwrap_or(true),
            include_markdown: args.include_markdown.unwrap_or(true),
            artifact_types: args.artifact_types.clone(),
            subsystem: args.subsystem.clone(),
            target_subsystem: args.target_subsystem.clone(),
        }
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
}

/// Returns true when a graph node's root passes the active root filter.
///
/// `None` filter matches any root. When a filter is set, the node must match
/// the slug (case-insensitive), OR be the synthetic "external" root (so that
/// external dependencies always appear in traversal results), OR be a
/// non-code slug (e.g., a memory root that stores markdown rather than code).
pub fn node_passes_root_filter(node_root: &str, root_filter: &Option<String>, non_code_slugs: &HashSet<String>) -> bool {
    match root_filter { None => true, Some(slug) => node_root.eq_ignore_ascii_case(slug) || node_root == "external" || non_code_slugs.contains(node_root) }
}

/// Returns true when an embedding search result passes the active root filter.
///
/// Non-code results (artifacts, markdown) always pass — they are not root-
/// scoped the same way code symbols are. Code results delegate to
/// `node_passes_root_filter` using the root prefix of the result ID.
pub fn search_result_passes_root_filter(result: &crate::embed::SearchResult, root_filter: &Option<String>, non_code_slugs: &HashSet<String>) -> bool {
    if root_filter.is_none() { return true; }
    if !result.kind.starts_with("code:") { return true; }
    node_passes_root_filter(result.id.split(':').next().unwrap_or(""), root_filter, non_code_slugs)
}
