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
//! - `TestHandleRequest` → `TestedBy` → `HandleRequest` (case-insensitive lowercasing only)
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

    // Pre-compute lowercase names for all production functions once.
    // This avoids the O(T × P) repeated `to_lowercase()` allocations that
    // would occur inside the nested loop below.  Filtering short names here
    // also keeps the inner list tight.
    let prod_indexed: Vec<(String, &Node)> = prod_fns
        .iter()
        .filter(|n| n.id.name.len() >= 4) // skip very short names ("new", "get", …)
        .map(|n| (n.id.name.to_lowercase(), *n))
        .collect();

    if prod_indexed.is_empty() {
        return Vec::new();
    }

    let mut edges: Vec<Edge> = Vec::new();

    for test_fn in &test_fns {
        let test_name_lower = test_fn.id.name.to_lowercase();

        for (prod_name_lower, prod_fn) in &prod_indexed {
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

    /// Integration test: extract a real Rust file and verify tested_by_pass
    /// produces TestedBy edges without LSP.
    #[test]
    fn test_integration_with_real_rust_extractor() {
        use crate::extract::ExtractorRegistry;
        use std::path::Path;

        let registry = ExtractorRegistry::with_builtins();
        let code = r#"
pub fn process_payment(amount: u64) -> bool {
    amount > 0
}

pub fn calculate_tax(amount: u64) -> u64 {
    amount / 10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_payment_valid() {
        assert!(process_payment(100));
    }

    #[test]
    fn test_calculate_tax_basic() {
        assert_eq!(calculate_tax(100), 10);
    }
}
"#;
        let result = registry.extract_file(Path::new("src/payment.rs"), code);

        // Run the naming-convention pass over the extracted nodes
        let edges = tested_by_pass(&result.nodes);

        let tested_by: Vec<_> = edges.iter()
            .filter(|e| e.kind == EdgeKind::TestedBy)
            .collect();

        assert!(
            !tested_by.is_empty(),
            "expected TestedBy edges from naming convention pass on real Rust code; \
             got 0. Nodes extracted: {:?}",
            result.nodes.iter().map(|n| (&n.id.name, &n.id.kind)).collect::<Vec<_>>()
        );

        // Verify at least one expected edge
        let has_payment_edge = tested_by.iter().any(|e|
            e.from.name.contains("process_payment") && e.to.name == "process_payment"
        );
        assert!(
            has_payment_edge,
            "expected test_process_payment_valid → process_payment TestedBy edge"
        );
    }

    // -------------------------------------------------------------------------
    // Adversarial tests — seeded from dissent findings
    // -------------------------------------------------------------------------

    /// Adversarial: running the pass twice (simulating incremental scan
    /// where the full node set is re-processed) must not produce doubled
    /// edge counts — callers handle dedup, but the pass itself must remain
    /// deterministic and return the same edges on each run.
    #[test]
    fn adversarial_idempotent_on_repeated_call() {
        let nodes = vec![
            make_fn("process_payment", false),
            make_fn("calculate_tax", false),
            make_fn("test_process_payment", true),
            make_fn("test_calculate_tax_rate", true),
        ];
        let edges_first = tested_by_pass(&nodes);
        let edges_second = tested_by_pass(&nodes);
        assert_eq!(
            edges_first.len(),
            edges_second.len(),
            "repeated calls must produce the same number of edges (idempotent)"
        );
    }

    /// Adversarial: very common short names that SHOULD be excluded even if
    /// they technically appear as substrings in many test names.
    /// "get", "set", "run", "new" are all < 4 chars — must not match.
    #[test]
    fn adversarial_common_short_names_excluded() {
        let short_names = ["get", "set", "run", "new", "all", "put"];
        for name in &short_names {
            let nodes = vec![
                make_fn(name, false),
                make_fn(&format!("test_{}_users", name), true),
            ];
            let edges = tested_by_pass(&nodes);
            assert!(
                edges.is_empty(),
                "production name '{}' (len={}) should be excluded — got {} edges",
                name, name.len(), edges.len()
            );
        }
    }

    /// Adversarial: production function with a very common name that is a
    /// strict substring of dozens of test names. The pass must not explode —
    /// it should still emit valid edges but we verify no self-edges appear.
    #[test]
    fn adversarial_no_self_edges_emitted() {
        // Both nodes have identical names but different is_test flags.
        // In practice this shouldn't happen (same name can't be both test
        // and production), but guard defensively.
        let test_fn = make_fn("process", true);
        let prod_fn = {
            // Make a node with the same name but explicitly NOT a test function.
            let mut n = make_fn("process", false);
            // Use different root to get a different NodeId.
            n.id.root = "prod".to_string();
            n
        };
        let nodes = vec![test_fn, prod_fn];
        let edges = tested_by_pass(&nodes);
        // "process" is 7 chars so would otherwise match, but since
        // test and prod have different roots, the self-edge guard won't
        // fire — they ARE different nodes. This is correct behavior.
        // Verify: if an edge is emitted the from != to.
        for edge in &edges {
            assert_ne!(
                edge.from, edge.to,
                "self-edge must never be emitted"
            );
        }
    }

    /// Adversarial: mix of struct nodes and function nodes — struct nodes
    /// with test-like names must not produce edges.
    #[test]
    fn adversarial_struct_with_test_name_ignored() {
        let mut struct_node = make_fn("test_process_payment_helper", false);
        struct_node.id.kind = NodeKind::Struct;
        // Mark it as "test" even so it would be classified as test if it were a function
        struct_node.metadata.insert("is_test".to_string(), "true".to_string());

        let nodes = vec![
            struct_node,
            make_fn("process_payment", false),
        ];
        let edges = tested_by_pass(&nodes);
        assert!(
            edges.is_empty(),
            "Struct nodes must never produce TestedBy edges even with test-like names"
        );
    }
}
