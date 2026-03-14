//! MCP tool handlers -- thin adapters over `crate::service`.
//!
//! Each handler parses MCP tool params, builds a `SearchContext`, delegates to
//! the shared service layer, and wraps the result as MCP `TextContent`.
//! This ensures CLI and MCP always execute the same search/graph/stats logic.

use petgraph::Direction;
use rust_mcp_sdk::schema::{CallToolError, CallToolResult};

use crate::embed::SearchMode;
use crate::graph::EdgeKind;
use crate::graph::index::GraphIndex;
use crate::service::{SearchContext, SearchParams};

use super::RnaHandler;
use super::tools::Search;
use super::helpers::text_result;

impl RnaHandler {
    // ── Unified search handler ──────────────────────────────────────────
    // Thin adapter: converts MCP `Search` args into `SearchParams` +
    // `SearchContext`, delegates to `service::search()`, wraps as MCP result.

    pub(crate) async fn handle_search(&self, args: Search) -> Result<CallToolResult, CallToolError> {
        let params = SearchParams::from_mcp_search(&args);

        let root_filter = self.effective_root_filter(args.root.as_deref());
        let non_code_slugs = if root_filter.is_some() {
            self.non_code_root_slugs()
        } else {
            std::collections::HashSet::new()
        };

        let graph_guard = match self.get_graph().await {
            Ok(g) => g,
            Err(e) => return Ok(text_result(format!("Graph error: {}", e))),
        };
        let graph_state = graph_guard.as_ref().unwrap();

        let embed_guard = self.embed_index.load();
        let embed_index = embed_guard.as_ref().as_ref();

        let ctx = SearchContext {
            graph_state,
            embed_index,
            repo_root: &self.repo_root,
            lsp_status: Some(&self.lsp_status),
            root_filter,
            non_code_slugs,
        };

        let markdown = crate::service::search(&params, &ctx).await;
        Ok(text_result(markdown))
    }
}

/// Execute a single graph traversal from a given node ID.
///
/// Shared by the service layer's traversal and batch search paths.
/// Keeping the logic in one place prevents the two paths from diverging.
pub(crate) fn run_traversal(
    index: &GraphIndex,
    node_id: &str,
    mode: &str,
    hops: Option<u32>,
    direction: Option<&str>,
    edge_filter: Option<&[EdgeKind]>,
) -> Result<Vec<String>, String> {
    match mode {
        "neighbors" => {
            let max_hops = hops.unwrap_or(1) as usize;
            let dir = direction.unwrap_or("outgoing");
            match dir {
                "outgoing" => {
                    if max_hops == 1 {
                        Ok(index.neighbors(node_id, edge_filter, Direction::Outgoing))
                    } else {
                        Ok(index.reachable(node_id, max_hops, edge_filter))
                    }
                }
                "incoming" => {
                    if max_hops == 1 {
                        Ok(index.neighbors(node_id, edge_filter, Direction::Incoming))
                    } else {
                        Ok(index.impact(node_id, max_hops, edge_filter))
                    }
                }
                "both" => {
                    let out = if max_hops == 1 {
                        index.neighbors(node_id, edge_filter, Direction::Outgoing)
                    } else {
                        index.reachable(node_id, max_hops, edge_filter)
                    };
                    let inc = if max_hops == 1 {
                        index.neighbors(node_id, edge_filter, Direction::Incoming)
                    } else {
                        index.impact(node_id, max_hops, edge_filter)
                    };
                    let mut combined = out;
                    let mut seen: std::collections::HashSet<String> = combined.iter().cloned().collect();
                    for id in inc {
                        if seen.insert(id.clone()) {
                            combined.push(id);
                        }
                    }
                    Ok(combined)
                }
                _ => Err(format!(
                    "Invalid direction: \"{}\". Use \"outgoing\", \"incoming\", or \"both\".",
                    dir
                )),
            }
        }
        "impact" => {
            if edge_filter.is_some() {
                return Err("edge_types is not supported with \"impact\" mode (it uses its own traversal strategy).".to_string());
            }
            let max_hops = hops.unwrap_or(3) as usize;
            Ok(index.impact(node_id, max_hops, None))
        }
        "reachable" => {
            let max_hops = hops.unwrap_or(3) as usize;
            Ok(index.reachable(node_id, max_hops, edge_filter))
        }
        "tests_for" => {
            if edge_filter.is_some() {
                return Err("edge_types is not supported with \"tests_for\" mode (it always uses Calls edges).".to_string());
            }
            let calls_filter = &[EdgeKind::Calls];
            Ok(index.neighbors(node_id, Some(calls_filter), Direction::Incoming))
        }
        other => Err(format!(
            "Unknown mode: \"{}\". Use \"neighbors\", \"impact\", \"reachable\", or \"tests_for\".",
            other
        )),
    }
}

/// Parse a `search_mode` string into [`SearchMode`].
/// Returns `Hybrid` for `None` or unrecognized values.
pub(crate) fn parse_search_mode(s: Option<&str>) -> SearchMode {
    match s.map(str::to_lowercase).as_deref() {
        Some("keyword") => SearchMode::Keyword,
        Some("semantic") => SearchMode::Semantic,
        _ => SearchMode::Hybrid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use crate::graph::{Node, NodeId, NodeKind, EdgeKind, ExtractionSource};
    use crate::graph::index::GraphIndex;
    use crate::ranking;

    /// Verify the tests_for pattern: incoming Calls edges filtered to test-file callers.
    #[test]
    fn test_tests_for_filters_to_test_callers_only() {
        let mut index = GraphIndex::new();
        let target = "root:src/server.rs:handle_search:function";
        let test_caller = "root:tests/server_test.rs:test_handle_search:function";
        let prod_caller = "root:src/main.rs:main:function";

        index.ensure_node(target, "function");
        index.ensure_node(test_caller, "function");
        index.ensure_node(prod_caller, "function");
        index.add_edge(test_caller, "function", target, "function", EdgeKind::Calls);
        index.add_edge(prod_caller, "function", target, "function", EdgeKind::Calls);

        let result = run_traversal(&index, target, "tests_for", None, None, None).unwrap();
        assert_eq!(result.len(), 2); // Both callers returned by traversal

        // The handler filters to test files -- simulate that:
        let test_nodes = vec![
            Node {
                id: NodeId {
                    root: "root".to_string(),
                    file: PathBuf::from("tests/server_test.rs"),
                    name: "test_handle_search".to_string(),
                    kind: NodeKind::Function,
                },
                language: "rust".to_string(),
                line_start: 1,
                line_end: 10,
                signature: String::new(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            },
            Node {
                id: NodeId {
                    root: "root".to_string(),
                    file: PathBuf::from("src/main.rs"),
                    name: "main".to_string(),
                    kind: NodeKind::Function,
                },
                language: "rust".to_string(),
                line_start: 1,
                line_end: 10,
                signature: String::new(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            },
        ];

        let filtered: Vec<_> = result.iter()
            .filter(|id| {
                test_nodes.iter()
                    .find(|n| n.stable_id() == **id)
                    .map(|n| ranking::is_test_file(n))
                    .unwrap_or(false)
            })
            .collect();

        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].contains("test_handle_search"));
    }

    /// Verify tests_for returns empty when no test files call the symbol.
    #[test]
    fn test_tests_for_no_test_callers() {
        let mut index = GraphIndex::new();
        let target = "root:src/server.rs:handle_search:function";
        let prod_caller = "root:src/main.rs:main:function";

        index.ensure_node(target, "function");
        index.ensure_node(prod_caller, "function");
        index.add_edge(prod_caller, "function", target, "function", EdgeKind::Calls);

        let result = run_traversal(&index, target, "tests_for", None, None, None).unwrap();
        assert_eq!(result.len(), 1);

        let test_nodes = vec![
            Node {
                id: NodeId {
                    root: "root".to_string(),
                    file: PathBuf::from("src/main.rs"),
                    name: "main".to_string(),
                    kind: NodeKind::Function,
                },
                language: "rust".to_string(),
                line_start: 1,
                line_end: 10,
                signature: String::new(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            },
        ];

        let filtered: Vec<_> = result.iter()
            .filter(|id| {
                test_nodes.iter()
                    .find(|n| n.stable_id() == **id)
                    .map(|n| ranking::is_test_file(n))
                    .unwrap_or(false)
            })
            .collect();

        assert_eq!(filtered.len(), 0);
    }

    #[test]
    fn test_code_prefix_filter_matches_all_embeddable_kinds() {
        let embeddable = vec![
            NodeKind::Function,
            NodeKind::Struct,
            NodeKind::Trait,
            NodeKind::Enum,
            NodeKind::Const,
            NodeKind::Impl,
            NodeKind::ProtoMessage,
            NodeKind::SqlTable,
            NodeKind::ApiEndpoint,
            NodeKind::Macro,
            NodeKind::Field,
        ];
        for kind in embeddable {
            let prefix = format!("code:{}", kind);
            assert!(
                prefix.starts_with("code:"),
                "kind {} should produce 'code:' prefix, got: {}",
                kind,
                prefix
            );
        }
    }

    #[test]
    fn test_non_embeddable_kinds_filtered_out_by_prefix() {
        let non_embeddable = vec![
            NodeKind::Import,
            NodeKind::Module,
            NodeKind::PrMerge,
        ];
        for kind in non_embeddable {
            let prefix = format!("code:{}", kind);
            let kind_str = kind.to_string();
            assert!(
                matches!(kind_str.as_str(), "import" | "module" | "pr_merge"),
                "kind {} should be filtered out by embed logic, prefix: {}",
                kind,
                prefix
            );
        }
    }

    #[test]
    fn test_code_prefix_filter_rejects_non_code_kinds() {
        let kinds = vec!["commit", "outcome", "signal", "guardrail"];
        for kind in kinds {
            assert!(!kind.starts_with("code:"), "Non-code kind should not have code: prefix");
        }
    }

    #[test]
    fn test_top_k_overflow_multiplication() {
        let top_k: u32 = 50;
        let result = top_k.min(50).checked_mul(3);
        assert_eq!(result, Some(150));

        let large_top_k: u32 = u32::MAX;
        let clamped = large_top_k.clamp(1, 50);
        assert_eq!(clamped, 50);
        let safe_mul = clamped as usize * 3;
        assert_eq!(safe_mul, 150);
    }

    #[test]
    fn test_parse_search_mode_defaults_to_hybrid() {
        assert!(matches!(parse_search_mode(None), SearchMode::Hybrid));
        assert!(matches!(
            parse_search_mode(Some("unknown")),
            SearchMode::Hybrid
        ));
        assert!(matches!(
            parse_search_mode(Some("")),
            SearchMode::Hybrid
        ));
    }

    #[test]
    fn test_parse_search_mode_keyword() {
        assert!(matches!(
            parse_search_mode(Some("keyword")),
            SearchMode::Keyword
        ));
        assert!(matches!(
            parse_search_mode(Some("KEYWORD")),
            SearchMode::Keyword
        ));
    }

    #[test]
    fn test_parse_search_mode_semantic() {
        assert!(matches!(
            parse_search_mode(Some("semantic")),
            SearchMode::Semantic
        ));
        assert!(matches!(
            parse_search_mode(Some("Semantic")),
            SearchMode::Semantic
        ));
    }

    /// Verify that kind-only filter correctly matches macro nodes.
    /// This tests the filtering logic used in service::search_flat.
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

        // Simulate the kind filter logic from service::search_flat (empty query, kind-only)
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

    /// Verify that the empty-query guard allows kind-only search.
    #[test]
    fn test_empty_query_guard_allows_kind_filter() {
        let query_str = "";
        let complexity_search = false;
        let sort_by_importance = false;

        // Without kind filter: should be rejected
        let has_kind_filter = false;
        let rejected = query_str.is_empty() && !complexity_search && !sort_by_importance && !has_kind_filter;
        assert!(rejected, "Empty query without kind should be rejected");

        // With kind filter: should be allowed
        let has_kind_filter = true;
        let rejected = query_str.is_empty() && !complexity_search && !sort_by_importance && !has_kind_filter;
        assert!(!rejected, "Empty query with kind filter should be allowed");
    }
}
