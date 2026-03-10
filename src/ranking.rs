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

        // Tier 5: more edges = more important
        let edge_count = |n: &Node| -> usize {
            let sid = n.stable_id();
            index.neighbors(&sid, None, Direction::Incoming).len()
                + index.neighbors(&sid, None, Direction::Outgoing).len()
        };
        edge_count(b).cmp(&edge_count(a))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, NodeId};
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
            byte_range: (0, 100),
            metadata: BTreeMap::new(),
        }
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

        let edges: Vec<Edge> = vec![];
        let index = GraphIndex::from_edges(&edges);
        let mut matches: Vec<&Node> = vec![&contains, &exact];
        sort_symbol_matches(&mut matches, "parse", &index);

        assert_eq!(matches[0].id.name, "parse");
        assert_eq!(matches[1].id.name, "parse_config");
    }

    #[test]
    fn test_sort_definitions_before_imports() {
        let func = make_node("foo", NodeKind::Function, "a.rs");
        let imp = make_node("foo", NodeKind::Import, "a.rs");

        let edges: Vec<Edge> = vec![];
        let index = GraphIndex::from_edges(&edges);
        let mut matches: Vec<&Node> = vec![&imp, &func];
        sort_symbol_matches(&mut matches, "foo", &index);

        assert_eq!(matches[0].id.kind, NodeKind::Function);
        assert_eq!(matches[1].id.kind, NodeKind::Import);
    }

    #[test]
    fn test_sort_non_test_before_test() {
        let prod = make_node("foo", NodeKind::Function, "src/lib.rs");
        let test = make_node("foo", NodeKind::Function, "tests/test_lib.rs");

        let edges: Vec<Edge> = vec![];
        let index = GraphIndex::from_edges(&edges);
        let mut matches: Vec<&Node> = vec![&test, &prod];
        sort_symbol_matches(&mut matches, "foo", &index);

        assert_eq!(
            matches[0].id.file,
            PathBuf::from("src/lib.rs")
        );
    }
}
