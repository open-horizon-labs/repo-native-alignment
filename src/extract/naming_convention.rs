//! Post-extraction pass that emits `TestedBy` edges from test functions to the
//! production functions they exercise, using naming conventions alone.
//!
//! # Problem
//!
//! RNA has a `mode="tests_for"` traversal but the first-class `TestedBy` edges
//! it relies on were only emitted inside the LSP enricher.  The LSP enricher
//! runs *after* tree-sitter extraction and may never fire on a fresh scan (LSP
//! startup delay) or on an incremental scan where only cached edges are loaded.
//! The result: zero `TestedBy` edges in the live graph unless LSP was fully
//! initialised.
//!
//! # Solution
//!
//! [`tested_by_pass`] runs as a post-extraction step alongside
//! [`api_link_pass`](super::api_link::api_link_pass).  It only needs
//! `Function` nodes — no LSP required — so it fires reliably on every scan.
//!
//! # Algorithm
//!
//! For every test function (identified by `is_test_function()`):
//!
//! 1. Convert the test name to lowercase.
//! 2. For every *production* function whose lowercase name is a substring of
//!    the test name:
//!    - Skip names shorter than 4 characters (avoids false positives from
//!      `new`, `get`, `run`, etc.).
//!    - Emit one `EdgeKind::TestedBy` edge: test_fn → prod_fn.
//!
//! Examples:
//! - `test_process_payment` → `TestedBy` → `process_payment`
//! - `TestHandleRequest` → `TestedBy` → `handle_request` (case-insensitive)
//! - `it_should_parse_config` → `TestedBy` → `parse_config`
//!
//! # Placement
//!
//! Call this **after all nodes from all roots have been merged** (i.e., after
//! `all_nodes` is fully populated in `build_full_graph_inner`, and after the
//! full node set is available in `update_graph_with_scan`).  This ensures
//! cross-file test/production pairs are linked even when only one side was
//! touched in an incremental scan.

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeKind};
use crate::ranking::is_test_function;

/// Post-extraction pass: emit `TestedBy` edges from test functions to the
/// production functions they cover, using naming-convention heuristics.
///
/// Call this after all nodes from all roots are merged so that cross-file
/// test/production pairs are discovered correctly during incremental scans.
///
/// Returns the new edges to add.  The returned `Vec` may be empty if no test
/// functions exist or no naming matches are found.
pub fn tested_by_pass(all_nodes: &[Node]) -> Vec<Edge> {
    // Partition into test and production functions in a single pass.
    let mut test_fns: Vec<&Node> = Vec::new();
    let mut prod_fns: Vec<&Node> = Vec::new();

    for node in all_nodes {
        if node.id.kind != NodeKind::Function {
            continue;
        }
        if is_test_function(node) {
            test_fns.push(node);
        } else {
            prod_fns.push(node);
        }
    }

    if test_fns.is_empty() || prod_fns.is_empty() {
        return Vec::new();
    }

    let mut edges: Vec<Edge> = Vec::new();

    for test_fn in &test_fns {
        let test_name_lower = test_fn.id.name.to_lowercase();

        for prod_fn in &prod_fns {
            // Guard: skip very short production names — they match too broadly.
            // "new", "get", "run", "put" are all < 4 chars.
            if prod_fn.id.name.len() < 4 {
                continue;
            }

            let prod_name_lower = prod_fn.id.name.to_lowercase();

            if test_name_lower.contains(prod_name_lower.as_str()) {
                // Defensive: never emit a self-edge.  The split above
                // (is_test_function / !is_test_function) makes this
                // unreachable in normal data, but guard anyway.
                if test_fn.id == prod_fn.id {
                    continue;
                }

                tracing::debug!(
                    "TestedBy (naming): {} -> {}",
                    test_fn.id.name,
                    prod_fn.id.name
                );

                edges.push(Edge {
                    from: test_fn.id.clone(),
                    to: prod_fn.id.clone(),
                    kind: EdgeKind::TestedBy,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }
        }
    }

    if !edges.is_empty() {
        tracing::info!(
            "TestedBy naming-convention pass: {} edge(s) from {} test function(s) \
             covering {} production function(s)",
            edges.len(),
            test_fns.len(),
            prod_fns.len()
        );
    }

    edges
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

    fn make_fn(name: &str, is_test: bool) -> Node {
        let mut meta = std::collections::BTreeMap::new();
        if is_test {
            meta.insert("is_test".to_string(), "true".to_string());
        }
        Node {
            id: NodeId {
                root: "r".into(),
                file: PathBuf::from("src/lib.rs"),
                name: name.into(),
                kind: NodeKind::Function,
            },
            language: "rust".into(),
            line_start: 1,
            line_end: 1,
            signature: format!("fn {}()", name),
            body: String::new(),
            metadata: meta,
            source: ExtractionSource::TreeSitter,
        }
    }

    #[test]
    fn test_basic_snake_case_match() {
        let nodes = vec![
            make_fn("process_payment", false),
            make_fn("test_process_payment", true),
        ];
        let edges = tested_by_pass(&nodes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from.name, "test_process_payment");
        assert_eq!(edges[0].to.name, "process_payment");
        assert_eq!(edges[0].kind, EdgeKind::TestedBy);
        assert_eq!(edges[0].source, ExtractionSource::TreeSitter);
    }

    #[test]
    fn test_camel_case_match() {
        let nodes = vec![
            make_fn("handleRequest", false),
            make_fn("TestHandleRequest", true),
        ];
        let edges = tested_by_pass(&nodes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from.name, "TestHandleRequest");
        assert_eq!(edges[0].to.name, "handleRequest");
    }

    #[test]
    fn test_short_name_skipped() {
        // "get" < 4 chars — must not produce an edge
        let nodes = vec![
            make_fn("get", false),
            make_fn("test_get_all", true),
        ];
        let edges = tested_by_pass(&nodes);
        assert!(edges.is_empty(), "names shorter than 4 chars must be skipped");
    }

    #[test]
    fn test_four_char_name_included() {
        // "send" is exactly 4 chars — must produce an edge
        let nodes = vec![
            make_fn("send", false),
            make_fn("test_send_message", true),
        ];
        let edges = tested_by_pass(&nodes);
        assert_eq!(edges.len(), 1, "4-char production names should be matched");
    }

    #[test]
    fn test_no_test_functions_returns_empty() {
        let nodes = vec![
            make_fn("process_payment", false),
            make_fn("handle_request", false),
        ];
        let edges = tested_by_pass(&nodes);
        assert!(edges.is_empty());
    }

    #[test]
    fn test_no_production_functions_returns_empty() {
        let nodes = vec![
            make_fn("test_process_payment", true),
        ];
        let edges = tested_by_pass(&nodes);
        assert!(edges.is_empty());
    }

    #[test]
    fn test_empty_nodes_returns_empty() {
        let edges = tested_by_pass(&[]);
        assert!(edges.is_empty());
    }

    #[test]
    fn test_case_insensitive_match() {
        // Test name lowercase contains production name lowercase
        let nodes = vec![
            make_fn("ParseConfig", false),
            make_fn("it_should_parseconfig_correctly", true),
        ];
        let edges = tested_by_pass(&nodes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to.name, "ParseConfig");
    }

    #[test]
    fn test_multiple_matches_for_one_test() {
        // One test whose name contains two production names
        let nodes = vec![
            make_fn("build", false),         // < 5 chars but == 5 actually, "build" is 5 — included
            make_fn("parse", false),         // 5 chars — included
            make_fn("test_build_and_parse", true),
        ];
        let edges = tested_by_pass(&nodes);
        // Both "build" and "parse" have len >= 4 and appear in the test name
        assert_eq!(edges.len(), 2, "should match both 'build' and 'parse'");
    }

    #[test]
    fn test_non_function_nodes_ignored() {
        let mut struct_node = make_fn("Payment", false);
        struct_node.id.kind = NodeKind::Struct;
        let nodes = vec![
            struct_node,
            make_fn("process_payment", false),
            make_fn("test_process_payment", true),
        ];
        let edges = tested_by_pass(&nodes);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].to.name, "process_payment");
    }

    #[test]
    fn test_edge_source_is_tree_sitter() {
        let nodes = vec![
            make_fn("process_payment", false),
            make_fn("test_process_payment", true),
        ];
        let edges = tested_by_pass(&nodes);
        assert_eq!(edges[0].source, ExtractionSource::TreeSitter,
            "source must be TreeSitter (not LSP) for the tree-sitter pass");
    }
}
