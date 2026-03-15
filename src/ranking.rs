//! Shared ranking logic for code symbol search results.
//!
//! Both `search` (flat mode) and its deprecated aliases rank code nodes using the same
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
        NodeKind::Function | NodeKind::Struct | NodeKind::Trait | NodeKind::Enum | NodeKind::TypeAlias | NodeKind::Macro => 0,
        NodeKind::Const | NodeKind::Field | NodeKind::Impl => 1,
        NodeKind::Import | NodeKind::Module => 2,
        _ => 1,
    }
}

/// Known trait impl method names that pollute repo_map top-N because they
/// accumulate many `Calls` edges (every `format!()` call creates an edge to `fmt`,
/// every `.clone()` to `clone`, etc.). Filtered from display only -- PageRank
/// scores stay accurate for graph traversal.
pub const TRAIT_IMPL_METHODS: &[&str] = &[
    "fmt", "clone", "drop", "default", "from", "into",
    "deref", "deref_mut", "eq", "hash", "partial_cmp", "cmp",
];

/// Returns true if a node looks like a trait impl method that should be
/// filtered from repo_map display. Checks the node name against
/// [`TRAIT_IMPL_METHODS`] and requires `NodeKind::Function`.
pub fn is_trait_impl_method(n: &Node) -> bool {
    if n.id.kind != NodeKind::Function {
        return false;
    }
    let name_lower = n.id.name.to_lowercase();
    TRAIT_IMPL_METHODS.iter().any(|&m| name_lower == m)
}

/// Returns true if a node is a test function (by decorator or file path).
///
/// Checks for `#[test]` in the decorators metadata, or falls back to
/// [`is_test_file`] for path-based detection.
pub fn is_test_function(n: &Node) -> bool {
    if n.id.kind != NodeKind::Function {
        return false;
    }
    // Check for #[test] decorator -- use word boundary matching to avoid
    // false positives on decorators like "attestation"
    if n.metadata.get("decorators").is_some_and(|d| {
        d.split(|c: char| c == ',' || c.is_whitespace())
            .any(|token| token.trim() == "test" || token.trim() == "tokio::test")
    }) {
        return true;
    }
    // Fall back to path-based detection
    is_test_file(n)
}

/// Returns true if a file path looks like a test or test-adjacent file.
///
/// This is the shared path-based check used by both `is_test_file()` (which takes
/// a `Node`) and `embed.rs` semantic search demotion (which operates on id strings).
///
/// Patterns recognised:
/// - JS/TS: `.test.` and `.spec.` anywhere in path (e.g. `config.test.ts`)
/// - Rust:  `_test.` and `_tests.` (e.g. `my_test.rs`, `helpers_tests.rs`)
/// - Rust:  `_spec.` (e.g. `parser_spec.rs`)
/// - Python: filename starts with `test_` (e.g. `test_utils.py`)
/// - Directories: `/test/`, `/tests/`, or root `test/`/`tests/`
/// - Test-adjacent: `smoke`, `bench`, `benchmark`, `fixture`, `fixtures` in path
pub fn is_test_path(p: &str) -> bool {
    let fname = p.rsplit('/').next().unwrap_or(p);
    // JS/TS conventions
    p.contains(".test.")
        || p.contains(".spec.")
        // Rust / general conventions (suffix before extension)
        || p.contains("_test.")
        || p.contains("_tests.")
        || p.contains("_spec.")
        // Directory-based
        || p.contains("/test/")
        || p.contains("/tests/")
        || p.starts_with("test/")
        || p.starts_with("tests/")
        // Python: test_ prefix on filename
        || fname.starts_with("test_")
        // Test-adjacent files: smoke tests, benchmarks, fixtures
        || fname.contains("smoke")
        || fname.contains("bench")
        || p.contains("/fixtures/")
        || p.contains("/fixture/")
        || p.starts_with("fixtures/")
        || p.starts_with("fixture/")
}

/// Returns true if the node lives in a test file (by path convention).
///
/// Thin wrapper around [`is_test_path`] that extracts the path from a `Node`.
pub fn is_test_file(n: &Node) -> bool {
    let p = n.id.file.to_string_lossy();
    is_test_path(&p)
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
            NodeKind::TypeAlias,
            NodeKind::Module,
            NodeKind::Import,
            NodeKind::Const,
            NodeKind::Impl,
            NodeKind::ProtoMessage,
            NodeKind::SqlTable,
            NodeKind::ApiEndpoint,
            NodeKind::Macro,
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

    #[test]
    fn test_macro_ranks_as_primary_definition() {
        let mac = make_node("my_macro", NodeKind::Macro, "a.rs");
        assert_eq!(kind_rank(&mac), 0, "Macro should be tier 0 (primary definition)");
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
    /// WITHOUT importance — the (Some(_), None) match arm ensures this
    /// regardless of edge counts (which are only consulted when neither
    /// node has a PageRank score).
    #[test]
    fn test_pagerank_node_beats_node_without_pagerank() {
        // Node with low PageRank score (0.1)
        let mut with_pr = make_node("hub", NodeKind::Function, "a.rs");
        with_pr.metadata.insert("importance".to_string(), "0.1".to_string());

        // Node without PageRank — falls back to edge count in (None, None) arm
        let without_pr = make_node("hub", NodeKind::Function, "a.rs");

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

    // ==================== is_test_file broadened patterns ====================

    #[test]
    fn test_is_test_file_rust_tests_suffix() {
        // _tests.rs (plural)
        let node = make_node("f", NodeKind::Function, "src/helpers_tests.rs");
        assert!(is_test_file(&node), "_tests.rs should be flagged as test file");
    }

    #[test]
    fn test_is_test_file_rust_spec_suffix() {
        let node = make_node("f", NodeKind::Function, "src/parser_spec.rs");
        assert!(is_test_file(&node), "_spec.rs should be flagged as test file");
    }

    #[test]
    fn test_is_test_file_python_test_prefix() {
        let node = make_node("f", NodeKind::Function, "src/test_utils.py");
        assert!(is_test_file(&node), "test_*.py should be flagged as test file");

        // In subdirectory
        let node2 = make_node("f", NodeKind::Function, "lib/test_parser.py");
        assert!(is_test_file(&node2), "lib/test_*.py should be flagged as test file");
    }

    #[test]
    fn test_is_test_file_python_no_false_positive() {
        // "testimony.py" should NOT match — test_ must be prefix of filename
        let node = make_node("f", NodeKind::Function, "src/testimony.py");
        assert!(!is_test_file(&node), "testimony.py should not be flagged as test file");
    }

    #[test]
    fn test_is_test_file_smoke() {
        let node = make_node("f", NodeKind::Function, "src/smoke.rs");
        assert!(is_test_file(&node), "smoke.rs should be flagged as test file");

        let node2 = make_node("f", NodeKind::Function, "src/smoke_test.rs");
        assert!(is_test_file(&node2), "smoke_test.rs should be flagged as test file");
    }

    #[test]
    fn test_is_test_file_bench() {
        let node = make_node("f", NodeKind::Function, "src/bench_ranking.rs");
        assert!(is_test_file(&node), "bench_*.rs should be flagged as test file");

        let node2 = make_node("f", NodeKind::Function, "benches/benchmark.rs");
        assert!(is_test_file(&node2), "benchmark.rs should be flagged as test file");
    }

    #[test]
    fn test_is_test_file_fixtures_directory() {
        let node = make_node("f", NodeKind::Function, "tests/fixtures/helper.rs");
        assert!(is_test_file(&node), "tests/fixtures/ should be flagged as test file");

        let node2 = make_node("f", NodeKind::Function, "fixture/data.py");
        assert!(is_test_file(&node2), "fixture/ at root should be flagged as test file");
    }

    #[test]
    fn test_is_test_file_no_false_positive_benchmark_dir() {
        // "benchmarks/main.rs" — bench is in filename via directory name
        // but the directory itself is not in our dir patterns. The filename
        // extraction would check the last component. Let's verify.
        let node = make_node("f", NodeKind::Function, "benchmarks/main.rs");
        // "main.rs" doesn't contain "bench" — this should NOT be flagged.
        assert!(!is_test_file(&node), "benchmarks/main.rs should not be flagged (filename is main.rs)");
    }

    #[test]
    fn test_is_test_file_root_test_prefix() {
        // File at root starting with test_
        let node = make_node("f", NodeKind::Function, "test_integration.py");
        assert!(is_test_file(&node), "root test_*.py should be flagged as test file");
    }

    // ==================== Adversarial: is_test_path edge cases ====================

    #[test]
    fn test_is_test_path_empty_string() {
        assert!(!is_test_path(""), "empty path should not be test file");
    }

    #[test]
    fn test_is_test_path_no_false_positive_protest_dir() {
        // "protest" contains "test" but not as "/test/" boundary
        assert!(!is_test_path("protest/main.rs"), "protest/ should not be flagged");
        assert!(!is_test_path("src/detest.rs"), "detest.rs should not be flagged");
    }

    #[test]
    fn test_is_test_path_direct_call_matches_is_test_file() {
        // Verify is_test_path and is_test_file agree on all cases
        let cases = vec![
            ("src/lib.rs", false),
            ("tests/foo.rs", true),
            ("src/smoke.rs", true),
            ("src/my_test.rs", true),
            ("src/bench_foo.rs", true),
            ("test_utils.py", true),
            ("attestation/lib.rs", false),
        ];
        for (path, expected) in cases {
            let node = make_node("f", NodeKind::Function, path);
            assert_eq!(
                is_test_path(path), expected,
                "is_test_path(\"{}\") should be {}",
                path, expected
            );
            assert_eq!(
                is_test_file(&node), expected,
                "is_test_file for \"{}\" should be {}",
                path, expected
            );
        }
    }

    // ==================== Trait impl method filtering ====================

    #[test]
    fn test_is_trait_impl_method_fmt() {
        let node = make_node("fmt", NodeKind::Function, "src/types.rs");
        assert!(is_trait_impl_method(&node), "fmt should be a trait impl method");
    }

    #[test]
    fn test_is_trait_impl_method_clone() {
        let node = make_node("clone", NodeKind::Function, "src/types.rs");
        assert!(is_trait_impl_method(&node), "clone should be a trait impl method");
    }

    #[test]
    fn test_is_trait_impl_method_from() {
        let node = make_node("from", NodeKind::Function, "src/types.rs");
        assert!(is_trait_impl_method(&node), "from should be a trait impl method");
    }

    #[test]
    fn test_is_trait_impl_method_case_insensitive() {
        let node = make_node("Fmt", NodeKind::Function, "src/types.rs");
        assert!(is_trait_impl_method(&node), "Fmt should match (case-insensitive)");
    }

    #[test]
    fn test_is_trait_impl_method_not_function() {
        let node = make_node("fmt", NodeKind::Struct, "src/types.rs");
        assert!(!is_trait_impl_method(&node), "struct named fmt should NOT be a trait impl method");
    }

    #[test]
    fn test_is_trait_impl_method_custom_name() {
        let node = make_node("process_data", NodeKind::Function, "src/main.rs");
        assert!(!is_trait_impl_method(&node), "process_data is not a trait impl method");
    }

    #[test]
    fn test_is_trait_impl_method_all_names() {
        for name in TRAIT_IMPL_METHODS {
            let node = make_node(name, NodeKind::Function, "src/types.rs");
            assert!(is_trait_impl_method(&node), "{} should be a trait impl method", name);
        }
    }

    // ==================== Test function detection ====================

    #[test]
    fn test_is_test_function_by_decorator() {
        let mut node = make_node("test_my_feature", NodeKind::Function, "src/main.rs");
        node.metadata.insert("decorators".to_string(), "test".to_string());
        assert!(is_test_function(&node), "function with #[test] decorator should be detected");
    }

    #[test]
    fn test_is_test_function_by_path() {
        let node = make_node("helper", NodeKind::Function, "tests/test_main.rs");
        assert!(is_test_function(&node), "function in test file should be detected");
    }

    #[test]
    fn test_is_test_function_production_code() {
        let node = make_node("process", NodeKind::Function, "src/main.rs");
        assert!(!is_test_function(&node), "production function should not be detected");
    }

    #[test]
    fn test_is_test_function_not_function_kind() {
        let mut node = make_node("TestStruct", NodeKind::Struct, "tests/test_main.rs");
        node.metadata.insert("decorators".to_string(), "test".to_string());
        assert!(!is_test_function(&node), "struct should not be detected even with test decorator");
    }
}
