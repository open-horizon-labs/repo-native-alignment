//! MCP tool handlers -- thin adapters over `crate::service`.
//!
//! Each handler parses MCP tool params, builds a service context, delegates to
//! the shared service layer, and wraps the result as MCP `TextContent`.

use petgraph::Direction;
use rust_mcp_sdk::schema::{CallToolError, CallToolResult};

use crate::embed::SearchMode;
use crate::graph::EdgeKind;
use crate::graph::index::GraphIndex;
use crate::service::{
    OutcomeProgressContext, OutcomeProgressParams, RepoMapContext, RepoMapParams, SearchContext,
    SearchParams,
};

use super::helpers::text_result;
use super::tools::{OutcomeProgress, RepoMap, Search};
use super::RnaHandler;

impl RnaHandler {
    pub(crate) async fn handle_search(&self, args: Search) -> Result<CallToolResult, CallToolError> {
        let params = SearchParams::from_mcp_search(&args);
        let root_filter = self.effective_root_filter(args.root.as_deref());
        let non_code_slugs = if root_filter.is_some() { self.non_code_root_slugs() } else { std::collections::HashSet::new() };
        let graph_guard = match self.get_graph().await { Ok(g) => g, Err(e) => return Ok(text_result(format!("Graph error: {}", e))), };
        let graph_state = graph_guard.as_ref().unwrap();
        let embed_guard = self.embed_index.load();
        let embed_index = embed_guard.as_ref().as_ref();
        let ctx = SearchContext { graph_state, embed_index, repo_root: &self.repo_root, lsp_status: Some(&self.lsp_status), root_filter, non_code_slugs };
        let markdown = crate::service::search(&params, &ctx).await;
        Ok(text_result(markdown))
    }

    pub(crate) async fn handle_outcome_progress(&self, args: OutcomeProgress) -> Result<CallToolResult, CallToolError> {
        let root_filter = self.effective_root_filter(args.root.as_deref());
        let non_code_slugs = if root_filter.is_some() { self.non_code_root_slugs() } else { std::collections::HashSet::new() };
        let graph_guard = match self.get_graph().await { Ok(g) => g, Err(e) => return Ok(text_result(format!("Graph error: {}", e))), };
        let graph_state = graph_guard.as_ref().unwrap();
        let params = OutcomeProgressParams { outcome_id: args.outcome_id, include_impact: args.include_impact.unwrap_or(false), root_filter, non_code_slugs };
        let ctx = OutcomeProgressContext { graph_state, repo_root: &self.repo_root };
        let markdown = crate::service::outcome_progress(&params, &ctx);
        Ok(text_result(markdown))
    }

    pub(crate) fn handle_list_roots(&self) -> Result<CallToolResult, CallToolError> {
        let markdown = crate::service::list_roots(&self.repo_root);
        Ok(text_result(markdown))
    }

    pub(crate) async fn handle_repo_map(&self, args: RepoMap) -> Result<CallToolResult, CallToolError> {
        let root_filter = self.effective_root_filter(args.root.as_deref());
        let non_code_slugs = if root_filter.is_some() { self.non_code_root_slugs() } else { std::collections::HashSet::new() };
        let graph_guard = match self.get_graph().await { Ok(g) => g, Err(e) => return Ok(text_result(format!("Graph error: {}", e))), };
        let graph_state = graph_guard.as_ref().unwrap();
        let params = RepoMapParams { top_n: args.top_n.unwrap_or(15) as usize, root_filter, non_code_slugs };
        let ctx = RepoMapContext { graph_state, repo_root: &self.repo_root, lsp_status: Some(&self.lsp_status) };
        let markdown = crate::service::repo_map(&params, &ctx);
        Ok(text_result(markdown))
    }
}

/// Execute a single graph traversal from a given node ID.
pub(crate) fn run_traversal(index: &GraphIndex, node_id: &str, mode: &str, hops: Option<u32>, direction: Option<&str>, edge_filter: Option<&[EdgeKind]>) -> Result<Vec<String>, String> {
    match mode {
        "neighbors" => {
            let max_hops = hops.unwrap_or(1) as usize;
            let dir = direction.unwrap_or("outgoing");
            match dir {
                "outgoing" => { if max_hops == 1 { Ok(index.neighbors(node_id, edge_filter, Direction::Outgoing)) } else { Ok(index.reachable(node_id, max_hops, edge_filter)) } }
                "incoming" => { if max_hops == 1 { Ok(index.neighbors(node_id, edge_filter, Direction::Incoming)) } else { Ok(index.impact(node_id, max_hops, edge_filter)) } }
                "both" => {
                    let out = if max_hops == 1 { index.neighbors(node_id, edge_filter, Direction::Outgoing) } else { index.reachable(node_id, max_hops, edge_filter) };
                    let inc = if max_hops == 1 { index.neighbors(node_id, edge_filter, Direction::Incoming) } else { index.impact(node_id, max_hops, edge_filter) };
                    let mut combined = out; let mut seen: std::collections::HashSet<String> = combined.iter().cloned().collect();
                    for id in inc { if seen.insert(id.clone()) { combined.push(id); } }
                    Ok(combined)
                }
                _ => Err(format!("Invalid direction: \"{}\". Use \"outgoing\", \"incoming\", or \"both\".", dir)),
            }
        }
        "impact" => { if edge_filter.is_some() { return Err("edge_types is not supported with \"impact\" mode.".to_string()); } let max_hops = hops.unwrap_or(3) as usize; Ok(index.impact(node_id, max_hops, None)) }
        "reachable" => { let max_hops = hops.unwrap_or(3) as usize; Ok(index.reachable(node_id, max_hops, edge_filter)) }
        "tests_for" => { if edge_filter.is_some() { return Err("edge_types is not supported with \"tests_for\" mode.".to_string()); } let calls_filter = &[EdgeKind::Calls]; Ok(index.neighbors(node_id, Some(calls_filter), Direction::Incoming)) }
        other => Err(format!("Unknown mode: \"{}\". Use \"neighbors\", \"impact\", \"reachable\", or \"tests_for\".", other)),
    }
}

/// Parse a `search_mode` string into [`SearchMode`].
pub(crate) fn parse_search_mode(s: Option<&str>) -> SearchMode {
    match s.map(str::to_lowercase).as_deref() { Some("keyword") => SearchMode::Keyword, Some("semantic") => SearchMode::Semantic, _ => SearchMode::Hybrid }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::index::GraphIndex;
    use crate::graph::{EdgeKind, ExtractionSource, Node, NodeId, NodeKind};
    use crate::ranking;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn test_tests_for_filters_to_test_callers_only() {
        let mut index = GraphIndex::new();
        let target = "root:src/server.rs:handle_search:function";
        let test_caller = "root:tests/server_test.rs:test_handle_search:function";
        let prod_caller = "root:src/main.rs:main:function";
        index.ensure_node(target, "function"); index.ensure_node(test_caller, "function"); index.ensure_node(prod_caller, "function");
        index.add_edge(test_caller, "function", target, "function", EdgeKind::Calls);
        index.add_edge(prod_caller, "function", target, "function", EdgeKind::Calls);
        let result = run_traversal(&index, target, "tests_for", None, None, None).unwrap();
        assert_eq!(result.len(), 2);
        let test_nodes = vec![
            Node { id: NodeId { root: "root".into(), file: PathBuf::from("tests/server_test.rs"), name: "test_handle_search".into(), kind: NodeKind::Function }, language: "rust".into(), line_start: 1, line_end: 10, signature: String::new(), body: String::new(), metadata: BTreeMap::new(), source: ExtractionSource::TreeSitter },
            Node { id: NodeId { root: "root".into(), file: PathBuf::from("src/main.rs"), name: "main".into(), kind: NodeKind::Function }, language: "rust".into(), line_start: 1, line_end: 10, signature: String::new(), body: String::new(), metadata: BTreeMap::new(), source: ExtractionSource::TreeSitter },
        ];
        let filtered: Vec<_> = result.iter().filter(|id| test_nodes.iter().find(|n| n.stable_id() == **id).map(ranking::is_test_file).unwrap_or(false)).collect();
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].contains("test_handle_search"));
    }

    #[test]
    fn test_parse_search_mode_variants() {
        assert!(matches!(parse_search_mode(None), SearchMode::Hybrid));
        assert!(matches!(parse_search_mode(Some("keyword")), SearchMode::Keyword));
        assert!(matches!(parse_search_mode(Some("semantic")), SearchMode::Semantic));
        assert!(matches!(parse_search_mode(Some("KEYWORD")), SearchMode::Keyword));
        assert!(matches!(parse_search_mode(Some("")), SearchMode::Hybrid));
    }
}
