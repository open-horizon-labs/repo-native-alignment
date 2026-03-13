//! Shared ranking logic for code symbol search results.
//!
//! Both `search_symbols` and `oh_search_context` rank code nodes using the same
//! 5-tier cascade. This module extracts that logic into a single function so
//! ranking changes propagate to all call sites without copy-paste drift.

use std::cmp::Ordering;

use crate::graph::index::GraphIndex;
use crate::graph::{Node, NodeKind};
use petgraph::Direction;

/// Rank a code symbol node by kind priority.
///
/// Lower value = higher priority. The tiers are:
/// - 0: Primary definitions (function, struct, trait, enum) — these are the
///       symbols developers most often search for.
/// - 1: Secondary definitions (const, field, impl) and any other kind — still
///       definitions but less commonly the direct search target.
/// - 2: Imports and modules — re-exports and namespace nodes. Ranked lowest
///       because they point to definitions rather than being definitions.
///
/// Note: there is no gap in values (0, 1, 2). Earlier versions used 0, 1, 3
/// which was a bug.
pub fn kind_rank(n: &Node) -> u8 {
    match n.id.kind {
        NodeKind::Function | NodeKind::Struct | NodeKind::Trait | NodeKind::Enum => 0,
        NodeKind::Const | NodeKind::Field | NodeKind::Impl => 1,
        NodeKind::Import | NodeKind::Module => 2,
        _ => 1,
    }
}

/// Returns true if the node lives in a test file (by path convention).
pub fn is_test_file(n: &Node) -> bool {
    let p = n.id.file.to_string_lossy();
    p.contains(".test.")
        || p.contains(".spec.")
        || p.contains("_test.")
        || p.contains("/test/")
        || p.contains("/tests/")
        || p.starts_with("test/")
        || p.starts_with("tests/")
}

/// Sort code symbol nodes by a 5-tier relevance cascade.
///
/// The tiers, in order of precedence:
///
/// 1. **Exact name match** — query == node name (case-insensitive). An exact
///    match is the strongest signal that the user found what they want.
///
/// 2. **Name contains query** — the node name includes the query as a substring,
///    vs. only appearing in the signature. Name matches are more specific than
///    signature-only matches (e.g. searching "parse" should prefer a function
///    named `parse_config` over one whose signature mentions "parse" in a comment).
///
/// 3. **Kind priority** — primary definitions (function, struct, trait, enum)
///    before secondary (const, field, impl) before imports/modules. Developers
///    searching for a name usually want the definition, not the re-export.
///
/// 4. **Non-test files first** — production code before test code. Tests are
///    useful but secondary when exploring an unfamiliar codebase.
///
/// 5. **Edge count (connectivity)** — symbols with more incoming + outgoing
///    edges are more central to the codebase and likely more relevant.
pub fn sort_symbol_matches<'a>(
    matches: &mut [&'a Node],
    query_lower: &str,
    index: &GraphIndex,
) {
    matches.sort_by(|a, b| {
        let a_name_lower = a.id.name.to_lowercase();
        let b_name_lower = b.id.name.to_lowercase();
        let a_exact = a_name_lower == query_lower;
        let b_exact = b_name_lower == query_lower;
        let a_name_contains = a_name_lower.contains(query_lower);
        let b_name_contains = b_name_lower.contains(query_lower);

        // Tier 1: exact name match
        match (a_exact, b_exact) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        // Tier 2: name contains vs signature-only
        match (a_name_contains, b_name_contains) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        // Tier 3: prefer definitions over imports
        let kr = kind_rank(a).cmp(&kind_rank(b));
        if kr != Ordering::Equal {
            return kr;
        }

        // Tier 4: non-test files before test files
        match (is_test_file(a), is_test_file(b)) {
            (false, true) => return Ordering::Less,
            (true, false) => return Ordering::Greater,
            _ => {}
        }

        // Tier 5: higher PageRank importance = more important.
        // Compare PageRank scores only when both nodes have them;
        // fall back to edge count only when neither does.
        // Never mix normalized [0,1] scores with raw degree counts.
        let pagerank = |n: &Node| {
            n.metadata.get("importance").and_then(|s| s.parse::<f64>().ok())
        };
        let edge_count = |n: &Node| {
            let sid = n.stable_id();
            index.neighbors(&sid, None, Direction::Incoming).len()
                + index.neighbors(&sid, None, Direction::Outgoing).len()
        };
        match (pagerank(a), pagerank(b)) {
            (Some(ai), Some(bi)) => bi.partial_cmp(&ai).unwrap_or(Ordering::Equal),
            (Some(_), None) => Ordering::Less,   // a has score, ranks higher
            (None, Some(_)) => Ordering::Greater, // b has score, ranks higher
            (None, None) => edge_count(b).cmp(&edge_count(a)),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ExtractionSource, NodeId};
    use crate::graph::index::GraphIndex;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_node(name: &str, kind: NodeKind, file: &str) -> Node {
        Node {
            id: NodeId {
                kind,
                name: name.to_string(),
                file: PathBuf::from(file),
                root: "local".to_string(),
            },
            language: "rust".to_string(),
            signature: format!("fn {}", name),
            line_start: 0,
            line_end: 10,
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn empty_index() -> GraphIndex {
        GraphIndex::new()
    }

    #[test]
    fn test_kind_rank_no_gap() {
        // Verify there's no gap in kind_rank values (was 0,1,3 — now 0,1,2)
        let func = make_node("f", NodeKind::Function, "a.rs");
        let cnst = make_node("c", NodeKind::Const, "a.rs");
        let imp = make_node("i", NodeKind::Import, "a.rs");

        assert_eq!(kind_rank(&func), 0);
        assert_eq!(kind_rank(&cnst), 1);
        assert_eq!(kind_rank(&imp), 2);
    }

    #[test]
    fn test_sort_exact_before_contains() {
        let exact = make_node("parse", NodeKind::Function, "a.rs");
        let contains = make_node("parse_config", NodeKind::Function, "a.rs");

        let index = empty_index();
        let mut matches: Vec<&Node> = vec![&contains, &exact];
        sort_symbol_matches(&mut matches, "parse", &index);

        assert_eq!(matches[0].id.name, "parse");
        assert_eq!(matches[1].id.name, "parse_config");
    }

    #[test]
    fn test_sort_definitions_before_imports() {
        let func = make_node("foo", NodeKind::Function, "a.rs");
        let imp = make_node("foo", NodeKind::Import, "a.rs");

        let index = empty_index();
        let mut matches: Vec<&Node> = vec![&imp, &func];
        sort_symbol_matches(&mut matches, "foo", &index);

        assert_eq!(matches[0].id.kind, NodeKind::Function);
        assert_eq!(matches[1].id.kind, NodeKind::Import);
    }

    #[test]
    fn test_sort_non_test_before_test() {
        let prod = make_node("foo", NodeKind::Function, "src/lib.rs");
        let test = make_node("foo", NodeKind::Function, "tests/test_lib.rs");

        let index = empty_index();
        let mut matches: Vec<&Node> = vec![&test, &prod];
        sort_symbol_matches(&mut matches, "foo", &index);

        assert_eq!(
            matches[0].id.file,
            PathBuf::from("src/lib.rs")
        );
    }

    // ==================== Adversarial tests ====================

    /// Empty query: `"".to_lowercase()` is `""`, and `"anything".contains("")`
    /// is true in Rust. So an empty query makes EVERY node an "exact match"
    /// (since `"foo" == ""` is false, but `"foo".contains("")` is true).
    /// Worse: `"" == ""` is true, so a node with empty name IS an exact match.
    /// This test documents the behavior.
    #[test]
    fn test_sort_empty_query_all_contain() {
        let a = make_node("alpha", NodeKind::Function, "a.rs");
        let b = make_node("beta", NodeKind::Function, "b.rs");

        let index = empty_index();
        let mut matches: Vec<&Node> = vec![&a, &b];
        // Empty query: both names "contain" it. Neither is "exact".
        // This should not panic.
        sort_symbol_matches(&mut matches, "", &index);
        assert_eq!(matches.len(), 2);
    }

    /// Empty-name node: a node with name "" gets `"" == ""` -> exact match
    /// for any empty-like query. Verify it doesn't crash sort.
    #[test]
    fn test_sort_empty_name_node() {
        let empty = Node {
            id: NodeId {
                kind: NodeKind::Function,
                name: "".to_string(),
                file: PathBuf::from("a.rs"),
                root: "local".to_string(),
            },
            language: "rust".to_string(),
            signature: "fn ()".to_string(),
            line_start: 0,
            line_end: 1,
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let normal = make_node("parse", NodeKind::Function, "b.rs");

        let index = empty_index();
        let mut matches: Vec<&Node> = vec![&empty, &normal];
        sort_symbol_matches(&mut matches, "parse", &index);

        // "parse" is exact match for normal, empty name doesn't contain "parse"
        assert_eq!(matches[0].id.name, "parse");
    }

    /// is_test_file false positive: "contested.rs" contains "_test." as a
    /// substring iff we're not careful — actually "_test." checks for the
    /// literal substring. "con_test_ed.rs" contains "_test_".
    /// But "contested.rs" does NOT contain "_test." — let's verify.
    #[test]
    fn test_is_test_file_false_positive_contested() {
        // "contested.rs" should NOT be detected as test file
        let node = make_node("f", NodeKind::Function, "src/contested.rs");
        assert!(
            !is_test_file(&node),
            "contested.rs should not be flagged as test file"
        );

        // But "my_test.rs" SHOULD be detected
        let test_node = make_node("f", NodeKind::Function, "src/my_test.rs");
        assert!(
            is_test_file(&test_node),
            "my_test.rs should be flagged as test file"
        );
    }

    /// is_test_file: path with "test" in directory but not as "/test/" pattern.
    /// e.g. "attestation/lib.rs" contains "test" but not "/test/".
    #[test]
    fn test_is_test_file_attestation_not_test() {
        let node = make_node("f", NodeKind::Function, "attestation/lib.rs");
        assert!(
            !is_test_file(&node),
            "attestation/lib.rs should not be flagged as test file"
        );
    }

    /// is_test_file: edge case with ".test." in filename (JS convention).
    #[test]
    fn test_is_test_file_js_convention() {
        let node = make_node("f", NodeKind::Function, "src/config.test.ts");
        assert!(is_test_file(&node));

        let node = make_node("f", NodeKind::Function, "src/config.spec.ts");
        assert!(is_test_file(&node));
    }

    /// Sorting stability: when all nodes have identical properties, the sort
    /// should produce a deterministic order across multiple runs.
    #[test]
    fn test_sort_stability_identical_nodes() {
        let nodes: Vec<Node> = (0..10)
            .map(|i| {
                let mut n = make_node("foo", NodeKind::Function, "a.rs");
                // Give them different signatures so we can tell them apart
                n.signature = format!("fn foo_{}", i);
                n
            })
            .collect();

        let index = empty_index();

        let first_order: Vec<String> = {
            let mut matches: Vec<&Node> = nodes.iter().collect();
            sort_symbol_matches(&mut matches, "foo", &index);
            matches.iter().map(|n| n.signature.clone()).collect()
        };

        for _ in 0..5 {
            let mut matches: Vec<&Node> = nodes.iter().collect();
            sort_symbol_matches(&mut matches, "foo", &index);
            let order: Vec<String> = matches.iter().map(|n| n.signature.clone()).collect();
            assert_eq!(first_order, order, "Sort is not stable across runs");
        }
    }

    /// kind_rank covers all NodeKind variants without panicking.
    #[test]
    fn test_kind_rank_all_variants() {
        let variants = vec![
            NodeKind::Function,
            NodeKind::Struct,
            NodeKind::Trait,
            NodeKind::Enum,
            NodeKind::Module,
            NodeKind::Import,
            NodeKind::Const,
            NodeKind::Impl,
            NodeKind::ProtoMessage,
            NodeKind::SqlTable,
            NodeKind::ApiEndpoint,
            NodeKind::Field,
            NodeKind::PrMerge,
        ];

        for kind in variants {
            let node = make_node("x", kind.clone(), "a.rs");
            let rank = kind_rank(&node);
            assert!(
                rank <= 2,
                "kind_rank({:?}) = {} — expected 0, 1, or 2",
                node.id.kind,
                rank
            );
        }
    }

    /// Tier precedence: exact name match should beat kind priority.
    /// An Import with exact name match should rank above a Function
    /// with only a contains match.
    #[test]
    fn test_sort_exact_match_beats_kind_priority() {
        let import_exact = make_node("parse", NodeKind::Import, "a.rs");
        let func_contains = make_node("parse_config", NodeKind::Function, "b.rs");

        let index = empty_index();
        let mut matches: Vec<&Node> = vec![&func_contains, &import_exact];
        sort_symbol_matches(&mut matches, "parse", &index);

        // Exact match (Import) should beat contains match (Function)
        // despite Import having worse kind_rank
        assert_eq!(
            matches[0].id.name, "parse",
            "Exact name match should beat better kind_rank with only contains match"
        );
        assert_eq!(matches[0].id.kind, NodeKind::Import);
    }

    /// Tier precedence: name-contains should beat kind priority.
    /// A Const with name-contains should rank above a Function with
    /// signature-only match.
    #[test]
    fn test_sort_name_contains_beats_kind() {
        let mut func_sig_only = make_node("do_stuff", NodeKind::Function, "a.rs");
        func_sig_only.signature = "fn do_stuff(config: Config)".to_string();
        let const_name = make_node("config_max", NodeKind::Const, "b.rs");

        let index = empty_index();
        let mut matches: Vec<&Node> = vec![&func_sig_only, &const_name];
        sort_symbol_matches(&mut matches, "config", &index);

        // const_name contains "config" in name, func only in signature
        assert_eq!(
            matches[0].id.name, "config_max",
            "Name-contains should beat signature-only match"
        );
    }

    /// Unicode node names: verify sorting works with non-ASCII identifiers.
    #[test]
    fn test_sort_unicode_names() {
        let node_a = make_node("Größe", NodeKind::Function, "a.rs");
        let node_b = make_node("größe_berechnen", NodeKind::Function, "b.rs");

        let index = empty_index();
        let mut matches: Vec<&Node> = vec![&node_b, &node_a];
        sort_symbol_matches(&mut matches, "größe", &index);

        // Exact match should come first
        assert_eq!(matches[0].id.name, "Größe");
    }

    // ==================== PageRank adversarial tests ====================

    /// Dissent finding #1: mixed-unit comparison.
    /// A node WITH PageRank importance should always rank above a node
    /// WITHOUT importance, even if the no-importance node has high edge count.
    #[test]
    fn test_pagerank_node_beats_high_degree_node_without_pagerank() {
        // Node with low PageRank score (0.1)
        let mut with_pr = make_node("hub", NodeKind::Function, "a.rs");
        with_pr.metadata.insert("importance".to_string(), "0.1".to_string());

        // Node without PageRank but would have high edge count (simulated)
        let without_pr = make_node("hub", NodeKind::Function, "a.rs");
        // No importance metadata — would fall back to edge count

        let index = empty_index();
        let mut matches: Vec<&Node> = vec![&without_pr, &with_pr];
        sort_symbol_matches(&mut matches, "hub", &index);

        // Node with PageRank should rank first (has importance, other doesn't)
        assert!(
            matches[0].metadata.contains_key("importance"),
            "Node with PageRank score should rank above node without, regardless of edge count"
        );
    }

    /// Dissent finding #1 (complement): when BOTH have PageRank, higher wins.
    #[test]
    fn test_pagerank_higher_score_ranks_first() {
        let mut high = make_node("hub", NodeKind::Function, "a.rs");
        high.metadata.insert("importance".to_string(), "0.9".to_string());

        let mut low = make_node("hub", NodeKind::Function, "a.rs");
        low.metadata.insert("importance".to_string(), "0.1".to_string());

        let index = empty_index();
        let mut matches: Vec<&Node> = vec![&low, &high];
        sort_symbol_matches(&mut matches, "hub", &index);

        let first_imp: f64 = matches[0].metadata.get("importance")
            .unwrap().parse().unwrap();
        assert!(
            first_imp > 0.5,
            "Higher PageRank (0.9) should rank before lower (0.1), got {:.1} first",
            first_imp
        );
    }

    /// Dissent finding #1 (complement): when NEITHER has PageRank,
    /// they should compare equally (both fall back to edge count in empty index = 0).
    #[test]
    fn test_no_pagerank_both_equal_no_panic() {
        let a = make_node("hub", NodeKind::Function, "a.rs");
        let b = make_node("hub", NodeKind::Function, "b.rs");

        let index = empty_index();
        let mut matches: Vec<&Node> = vec![&a, &b];
        sort_symbol_matches(&mut matches, "hub", &index);
        assert_eq!(matches.len(), 2); // no panic, no filtering
    }
}
