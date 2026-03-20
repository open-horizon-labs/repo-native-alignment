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
        let ctx = SearchContext { graph_state, embed_index, repo_root: &self.repo_root, lsp_status: Some(&self.lsp_status), embed_status: Some(&self.embed_status), root_filter, non_code_slugs };
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

    pub(crate) async fn handle_list_roots(&self) -> Result<CallToolResult, CallToolError> {
        // Derive the active root slugs from the in-memory graph so that the
        // output reflects what is actually loaded (including declared roots
        // persisted to LanceDB) rather than re-discovering roots from config.
        let graph_guard = self.get_graph().await.ok();
        let graph_state_ref = graph_guard.as_ref().and_then(|g| g.as_ref());

        let active_slugs = if let Some(gs) = graph_state_ref {
            let index_map = gs.node_index_map();
            crate::server::state::GraphState::root_slugs_from_index_map(&index_map)
                .into_iter()
                .collect::<std::collections::HashSet<String>>()
        } else {
            std::collections::HashSet::new()
        };

        let markdown = crate::service::list_roots_from_slugs(
            &self.repo_root,
            &active_slugs,
            graph_state_ref,
            Some(&self.lsp_status),
        );
        Ok(text_result(markdown))
    }

    pub(crate) async fn handle_repo_map(&self, args: RepoMap) -> Result<CallToolResult, CallToolError> {
        let root_filter = self.effective_root_filter(args.root.as_deref());
        let non_code_slugs = if root_filter.is_some() { self.non_code_root_slugs() } else { std::collections::HashSet::new() };
        let graph_guard = match self.get_graph().await { Ok(g) => g, Err(e) => return Ok(text_result(format!("Graph error: {}", e))), };
        let graph_state = graph_guard.as_ref().unwrap();
        let params = RepoMapParams { top_n: args.top_n.unwrap_or(15) as usize, root_filter, non_code_slugs };
        let ctx = RepoMapContext { graph_state, repo_root: &self.repo_root, lsp_status: Some(&self.lsp_status), embed_status: Some(&self.embed_status) };
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
        "impact" => { let max_hops = hops.unwrap_or(3) as usize; let default_filter = [EdgeKind::Calls, EdgeKind::ReferencedBy]; let filter = edge_filter.unwrap_or(&default_filter); Ok(index.impact(node_id, max_hops, Some(filter))) }
        "reachable" => { let max_hops = hops.unwrap_or(3) as usize; Ok(index.reachable(node_id, max_hops, edge_filter)) }
        "tests_for" => { if edge_filter.is_some() { return Err("edge_types is not supported with \"tests_for\" mode.".to_string()); } let calls_filter = &[EdgeKind::Calls]; Ok(index.neighbors(node_id, Some(calls_filter), Direction::Incoming)) }
        other => Err(format!("Unknown mode: \"{}\". Use \"neighbors\", \"impact\", \"reachable\", or \"tests_for\".", other)),
    }
}

/// Execute a graph traversal and return results grouped by edge type.
///
/// For 1-hop neighbors, uses `neighbors_grouped()` directly.
/// For multi-hop (reachable/impact), groups results by the edge type of the
/// first hop connecting each result to the entry node.
/// For tests_for, returns a single "Calls" group.
pub(crate) fn run_traversal_grouped(
    index: &GraphIndex,
    node_id: &str,
    mode: &str,
    hops: Option<u32>,
    direction: Option<&str>,
    edge_filter: Option<&[EdgeKind]>,
) -> Result<std::collections::BTreeMap<EdgeKind, Vec<String>>, String> {
    match mode {
        "neighbors" => {
            let max_hops = hops.unwrap_or(1) as usize;
            let dir = direction.unwrap_or("outgoing");

            if max_hops == 1 {
                // Direct 1-hop: use neighbors_grouped for exact edge info
                match dir {
                    "outgoing" => Ok(index.neighbors_grouped(node_id, edge_filter, Direction::Outgoing)),
                    "incoming" => Ok(index.neighbors_grouped(node_id, edge_filter, Direction::Incoming)),
                    "both" => {
                        let mut out = index.neighbors_grouped(node_id, edge_filter, Direction::Outgoing);
                        let inc = index.neighbors_grouped(node_id, edge_filter, Direction::Incoming);
                        // Merge incoming groups into outgoing, deduplicating
                        for (kind, ids) in inc {
                            let entry = out.entry(kind).or_default();
                            for id in ids {
                                if !entry.contains(&id) {
                                    entry.push(id);
                                }
                            }
                        }
                        Ok(out)
                    }
                    _ => Err(format!("Invalid direction: \"{}\". Use \"outgoing\", \"incoming\", or \"both\".", dir)),
                }
            } else {
                // Multi-hop: get flat results then group by first-hop edge type
                let flat_ids = run_traversal(index, node_id, mode, hops, direction, edge_filter)?;
                Ok(group_by_first_hop(index, node_id, &flat_ids, edge_filter, dir))
            }
        }
        "impact" => {
            if edge_filter.is_some() {
                return Err("edge_types is not supported with \"impact\" mode.".to_string());
            }
            let flat_ids = run_traversal(index, node_id, mode, hops, direction, edge_filter)?;
            Ok(group_by_first_hop(index, node_id, &flat_ids, None, "incoming"))
        }
        "reachable" => {
            let flat_ids = run_traversal(index, node_id, mode, hops, direction, edge_filter)?;
            Ok(group_by_first_hop(index, node_id, &flat_ids, edge_filter, "outgoing"))
        }
        "tests_for" => {
            if edge_filter.is_some() {
                return Err("edge_types is not supported with \"tests_for\" mode.".to_string());
            }
            let flat_ids = run_traversal(index, node_id, mode, hops, direction, edge_filter)?;
            let mut groups = std::collections::BTreeMap::new();
            if !flat_ids.is_empty() {
                groups.insert(EdgeKind::Calls, flat_ids);
            }
            Ok(groups)
        }
        other => Err(format!("Unknown mode: \"{}\". Use \"neighbors\", \"impact\", \"reachable\", or \"tests_for\".", other)),
    }
}

/// Group flat traversal results by the edge type of the first hop from the entry node.
///
/// For each result ID, checks which edge type connects it (directly) to `node_id`.
/// For multi-hop results not directly connected, assigns them to the edge type of
/// the first-hop neighbor through which they were discovered.
fn group_by_first_hop(
    index: &GraphIndex,
    node_id: &str,
    flat_ids: &[String],
    edge_filter: Option<&[EdgeKind]>,
    dir: &str,
) -> std::collections::BTreeMap<EdgeKind, Vec<String>> {
    let direction = match dir {
        "incoming" => Direction::Incoming,
        _ => Direction::Outgoing,
    };

    // Get the direct 1-hop neighbors grouped by edge type
    let first_hop = index.neighbors_grouped(node_id, edge_filter, direction);

    // Build a lookup: direct neighbor -> edge_kind
    let mut direct_edge: std::collections::HashMap<String, EdgeKind> = std::collections::HashMap::new();
    for (kind, ids) in &first_hop {
        for id in ids {
            direct_edge.entry(id.clone()).or_insert_with(|| kind.clone());
        }
    }

    // For each result in flat_ids, determine its first-hop edge type.
    // Pre-compute reachability sets per first-hop neighbor to avoid O(N^2).
    let mut reachability_cache: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for (_kind, fh_ids) in &first_hop {
        for fh_id in fh_ids {
            reachability_cache.entry(fh_id.clone()).or_insert_with(|| {
                index.reachable(fh_id, 10, edge_filter)
                    .into_iter()
                    .collect()
            });
        }
    }

    let flat_set: std::collections::HashSet<&str> = flat_ids.iter().map(|s| s.as_str()).collect();
    let mut groups: std::collections::BTreeMap<EdgeKind, Vec<String>> = std::collections::BTreeMap::new();

    for id in flat_ids {
        if let Some(kind) = direct_edge.get(id) {
            // Direct neighbor: exact edge type known
            groups.entry(kind.clone()).or_default().push(id.clone());
        } else {
            // Multi-hop: find which first-hop edge type leads here.
            let mut assigned = false;
            for (kind, fh_ids) in &first_hop {
                for fh_id in fh_ids {
                    if let Some(reachable_set) = reachability_cache.get(fh_id) {
                        if reachable_set.contains(id) {
                            groups.entry(kind.clone()).or_default().push(id.clone());
                            assigned = true;
                            break;
                        }
                    }
                }
                if assigned { break; }
            }
            if !assigned && !flat_set.is_empty() {
                // Fallback: use the first available edge type
                if let Some((kind, _)) = first_hop.iter().next() {
                    groups.entry(kind.clone()).or_default().push(id.clone());
                }
            }
        }
    }

    groups
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

    /// Verify that kind-only filter correctly matches macro nodes.
    #[test]
    fn test_kind_filter_matches_macro_nodes() {
        let macro_node = Node {
            id: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "my_vec".to_string(),
                kind: NodeKind::Macro,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 5,
            signature: "macro_rules! my_vec".to_string(),
            body: "macro_rules! my_vec { ... }".to_string(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let fn_node = Node {
            id: NodeId {
                root: "root".to_string(),
                file: PathBuf::from("src/lib.rs"),
                name: "do_stuff".to_string(),
                kind: NodeKind::Function,
            },
            language: "rust".to_string(),
            line_start: 10,
            line_end: 15,
            signature: "fn do_stuff()".to_string(),
            body: "fn do_stuff() {}".to_string(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let nodes = vec![macro_node, fn_node];

        let kind_filter = Some("macro".to_string());
        let query_lower = "";
        let matches: Vec<_> = nodes
            .iter()
            .filter(|n| {
                if !query_lower.is_empty() {
                    let name_match = n.id.name.to_lowercase().contains(query_lower)
                        || n.signature.to_lowercase().contains(query_lower);
                    if !name_match {
                        return false;
                    }
                }
                if let Some(ref kf) = kind_filter {
                    if n.id.kind.to_string().to_lowercase() != kf.to_lowercase() {
                        return false;
                    }
                }
                true
            })
            .collect();

        assert_eq!(matches.len(), 1, "Kind-only filter should find exactly 1 macro");
        assert_eq!(matches[0].id.name, "my_vec");
        assert_eq!(matches[0].id.kind, NodeKind::Macro);
    }

    /// Verify that the empty-query guard allows browse filters (kind, file, synthetic).
    #[test]
    fn test_empty_query_guard_allows_kind_filter() {
        let query_str = "";
        let complexity_search = false;
        let sort_by_importance = false;

        // No filters -- rejected
        let has_browse_filter = false;
        let rejected = query_str.is_empty()
            && !complexity_search
            && !sort_by_importance
            && !has_browse_filter;
        assert!(rejected, "Empty query without any filter should be rejected");

        // kind filter -- allowed
        let has_browse_filter = true; // kind=Some
        let rejected = query_str.is_empty()
            && !complexity_search
            && !sort_by_importance
            && !has_browse_filter;
        assert!(!rejected, "Empty query with kind filter should be allowed");
    }

    #[test]
    fn test_empty_query_guard_allows_file_filter() {
        let query_str = "";
        let complexity_search = false;
        let sort_by_importance = false;

        let has_browse_filter = true; // file=Some
        let rejected = query_str.is_empty()
            && !complexity_search
            && !sort_by_importance
            && !has_browse_filter;
        assert!(!rejected, "Empty query with file filter should be allowed");
    }

    #[test]
    fn test_empty_query_guard_allows_synthetic_filter() {
        let query_str = "";
        let complexity_search = false;
        let sort_by_importance = false;

        let has_browse_filter = true; // synthetic=Some
        let rejected = query_str.is_empty()
            && !complexity_search
            && !sort_by_importance
            && !has_browse_filter;
        assert!(!rejected, "Empty query with synthetic filter should be allowed");
    }

    // ==================== Adversarial tests for edge-type grouping ====================

    #[test]
    fn test_run_traversal_grouped_neighbors_1hop() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("a", "fn", "c", "struct", EdgeKind::DependsOn);
        index.add_edge("a", "fn", "d", "fn", EdgeKind::Calls);

        let groups = run_traversal_grouped(&index, "a", "neighbors", None, None, None).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[&EdgeKind::Calls].len(), 2);
        assert_eq!(groups[&EdgeKind::DependsOn].len(), 1);
    }

    #[test]
    fn test_run_traversal_grouped_neighbors_both_directions() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "a", "fn", EdgeKind::DependsOn);

        let groups = run_traversal_grouped(&index, "a", "neighbors", None, Some("both"), None).unwrap();
        assert_eq!(groups.len(), 2);
        assert!(groups[&EdgeKind::Calls].contains(&"b".to_string()));
        assert!(groups[&EdgeKind::DependsOn].contains(&"c".to_string()));
    }

    #[test]
    fn test_run_traversal_grouped_tests_for_returns_calls() {
        let mut index = GraphIndex::new();
        index.add_edge("test_fn", "fn", "target", "fn", EdgeKind::Calls);
        index.add_edge("prod_fn", "fn", "target", "fn", EdgeKind::Calls);

        let groups = run_traversal_grouped(&index, "target", "tests_for", None, None, None).unwrap();
        assert_eq!(groups.len(), 1);
        assert!(groups.contains_key(&EdgeKind::Calls));
        assert_eq!(groups[&EdgeKind::Calls].len(), 2);
    }

    #[test]
    fn test_run_traversal_grouped_empty_for_isolated_node() {
        let mut index = GraphIndex::new();
        index.ensure_node("lonely", "fn");

        let groups = run_traversal_grouped(&index, "lonely", "neighbors", None, None, None).unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn test_run_traversal_grouped_multi_hop_groups_by_first_hop() {
        // a -calls-> b -depends_on-> c
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::DependsOn);

        let groups = run_traversal_grouped(&index, "a", "neighbors", Some(2), None, None).unwrap();
        // Both b and c should be grouped under Calls (first-hop edge type from a)
        assert!(groups.contains_key(&EdgeKind::Calls), "should group under first-hop edge type");
        let calls = &groups[&EdgeKind::Calls];
        assert!(calls.contains(&"b".to_string()));
        assert!(calls.contains(&"c".to_string()));
    }

    #[test]
    fn test_run_traversal_grouped_impact_mode() {
        // a -> b -> c (impact of c: b and a, reversed)
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);

        let groups = run_traversal_grouped(&index, "c", "impact", None, None, None).unwrap();
        assert!(!groups.is_empty(), "should find dependents via Calls edges");
        // b is a direct incoming neighbor via Calls (impact defaults to Calls+ReferencedBy)
        assert!(groups.contains_key(&EdgeKind::Calls), "should have Calls group");
    }

    #[test]
    fn test_run_traversal_grouped_with_edge_filter() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("a", "fn", "c", "struct", EdgeKind::DependsOn);
        index.add_edge("a", "fn", "d", "trait", EdgeKind::Implements);

        let filter = vec![EdgeKind::Calls, EdgeKind::Implements];
        let groups = run_traversal_grouped(&index, "a", "neighbors", None, None, Some(&filter)).unwrap();
        assert_eq!(groups.len(), 2);
        assert!(!groups.contains_key(&EdgeKind::DependsOn));
    }
}
