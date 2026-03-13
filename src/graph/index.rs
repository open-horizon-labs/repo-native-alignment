//! In-memory petgraph index for structural graph traversal.
//!
//! The `GraphIndex` is a derived index rebuilt from LanceDB edge data.
//! It provides fast BFS/DFS traversal, neighbor queries, and impact
//! analysis that would be expensive as columnar joins.

use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::algo::page_rank;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef as PetgraphEdgeRef;
use petgraph::Direction;

use super::{Edge, EdgeKind};

// ---------------------------------------------------------------------------
// Lightweight references stored in petgraph (not full Node/Edge structs)
// ---------------------------------------------------------------------------

/// Lightweight node reference stored in petgraph. Contains just enough
/// to identify the node and look it up in LanceDB.
#[derive(Debug, Clone)]
pub struct NodeRef {
    /// The deterministic string ID (matches LanceDB `id` column).
    pub id: String,
    /// Node type string (e.g., "function", "struct", "proto_message").
    pub node_type: String,
}

/// Lightweight edge reference stored in petgraph.
#[derive(Debug, Clone)]
pub struct EdgeRef {
    pub edge_type: EdgeKind,
}

// ---------------------------------------------------------------------------
// GraphIndex
// ---------------------------------------------------------------------------

/// In-memory directed graph index backed by petgraph.
///
/// This is a derived cache rebuilt from LanceDB edge data. If it drifts,
/// rebuild it. The design intentionally keeps petgraph as a throwaway index
/// rather than a source of truth.
pub struct GraphIndex {
    graph: DiGraph<NodeRef, EdgeRef>,
    node_lookup: HashMap<String, NodeIndex>,
}

impl GraphIndex {
    /// Create an empty graph index.
    pub fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            node_lookup: HashMap::new(),
        }
    }

    /// Number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Ensure a node exists in the graph. Returns its index.
    /// If the node already exists, returns the existing index.
    pub fn ensure_node(&mut self, id: &str, node_type: &str) -> NodeIndex {
        if let Some(&idx) = self.node_lookup.get(id) {
            return idx;
        }
        let idx = self.graph.add_node(NodeRef {
            id: id.to_string(),
            node_type: node_type.to_string(),
        });
        self.node_lookup.insert(id.to_string(), idx);
        idx
    }

    /// Add a directed edge between two nodes. Creates nodes if they don't exist.
    pub fn add_edge(
        &mut self,
        from_id: &str,
        from_type: &str,
        to_id: &str,
        to_type: &str,
        edge_type: EdgeKind,
    ) {
        let from_idx = self.ensure_node(from_id, from_type);
        let to_idx = self.ensure_node(to_id, to_type);
        self.graph.add_edge(from_idx, to_idx, EdgeRef { edge_type });
    }

    /// Rebuild the graph from a slice of `Edge` structs (e.g., loaded from LanceDB).
    /// Clears the existing graph first.
    pub fn rebuild_from_edges(&mut self, edges: &[Edge]) {
        self.graph.clear();
        self.node_lookup.clear();

        for edge in edges {
            let from_id = edge.from.to_stable_id();
            let to_id = edge.to.to_stable_id();
            self.add_edge(
                &from_id,
                &edge.from.kind.to_string(),
                &to_id,
                &edge.to.kind.to_string(),
                edge.kind.clone(),
            );
        }
    }

    /// Get the NodeRef for a given node ID, if it exists.
    pub fn get_node(&self, id: &str) -> Option<&NodeRef> {
        self.node_lookup
            .get(id)
            .map(|&idx| &self.graph[idx])
    }

    /// 1-hop filtered neighbors of a node.
    ///
    /// - `edge_types`: if `Some`, only follow edges of these types. If `None`, follow all.
    /// - `direction`: `Outgoing` for callees, `Incoming` for callers, etc.
    ///
    /// Returns the IDs of neighboring nodes.
    pub fn neighbors(
        &self,
        node_id: &str,
        edge_types: Option<&[EdgeKind]>,
        direction: Direction,
    ) -> Vec<String> {
        let Some(&idx) = self.node_lookup.get(node_id) else {
            return Vec::new();
        };

        let mut result = Vec::new();
        let edges = self.graph.edges_directed(idx, direction);

        for edge_ref in edges {
            // Filter by edge type if specified
            if let Some(types) = edge_types {
                if !types.contains(&edge_ref.weight().edge_type) {
                    continue;
                }
            }

            let neighbor_idx = match direction {
                Direction::Outgoing => edge_ref.target(),
                Direction::Incoming => edge_ref.source(),
            };
            result.push(self.graph[neighbor_idx].id.clone());
        }

        result
    }

    /// BFS reachability within N hops, optionally filtered by edge types.
    ///
    /// Returns all node IDs reachable from `node_id` within `max_hops` edges,
    /// following only `Outgoing` edges. Does not include the start node.
    pub fn reachable(
        &self,
        node_id: &str,
        max_hops: usize,
        edge_types: Option<&[EdgeKind]>,
    ) -> Vec<String> {
        let Some(&start_idx) = self.node_lookup.get(node_id) else {
            return Vec::new();
        };

        let mut visited = HashSet::new();
        visited.insert(start_idx);

        let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
        queue.push_back((start_idx, 0));

        let mut result = Vec::new();

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_hops {
                continue;
            }

            for edge_ref in self.graph.edges_directed(current, Direction::Outgoing) {
                if let Some(types) = edge_types {
                    if !types.contains(&edge_ref.weight().edge_type) {
                        continue;
                    }
                }

                let neighbor = edge_ref.target();
                if visited.insert(neighbor) {
                    result.push(self.graph[neighbor].id.clone());
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }

        result
    }

    /// Reverse BFS: "what depends on this node?"
    ///
    /// Follows `Incoming` edges to find all nodes that transitively depend
    /// on the given node, within `max_hops`. Does not include the start node.
    pub fn impact(
        &self,
        node_id: &str,
        max_hops: usize,
    ) -> Vec<String> {
        let Some(&start_idx) = self.node_lookup.get(node_id) else {
            return Vec::new();
        };

        let mut visited = HashSet::new();
        visited.insert(start_idx);

        let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
        queue.push_back((start_idx, 0));

        let mut result = Vec::new();

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_hops {
                continue;
            }

            for edge_ref in self.graph.edges_directed(current, Direction::Incoming) {
                let neighbor = edge_ref.source();
                if visited.insert(neighbor) {
                    result.push(self.graph[neighbor].id.clone());
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }

        result
    }

    /// Compute PageRank importance scores for all nodes in the graph.
    ///
    /// Uses the standard PageRank algorithm with the given damping factor
    /// (typically 0.85) and number of iterations. Returns a map from
    /// node stable ID to importance score.
    ///
    /// Scores are max-normalized to [0, 1] where 1.0 is the most important
    /// node. These are relative ranks, not probabilities — they do not sum
    /// to 1.0. This makes scores interpretable regardless of graph size.
    ///
    /// After normalization, boilerplate symbols are dampened so that
    /// standard trait impls (`fmt`, `default`, `clone`, etc.), field nodes,
    /// and test-file symbols don't dominate the top-N. See
    /// [`GraphIndex::dampen_boilerplate`] for the heuristics applied.
    pub fn compute_pagerank(&self, damping_factor: f64, nb_iter: usize) -> HashMap<String, f64> {
        let raw_scores = page_rank(&self.graph, damping_factor, nb_iter);

        // Find max score for normalization
        let max_score = raw_scores
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);

        let mut result = HashMap::new();
        if max_score <= 0.0 {
            return result;
        }

        for (idx, &score) in raw_scores.iter().enumerate() {
            let node_index = NodeIndex::new(idx);
            if let Some(node_ref) = self.graph.node_weight(node_index) {
                result.insert(node_ref.id.clone(), score / max_score);
            }
        }

        // Apply boilerplate dampening so trait impls and field nodes don't
        // dominate architectural hubs in repo_map and importance sorting.
        self.dampen_boilerplate(&mut result);

        result
    }

    /// Dampen PageRank scores for known boilerplate patterns.
    ///
    /// Standard trait impls (`fmt`, `default`, `clone`, `eq`, `hash`, etc.)
    /// accumulate disproportionate PageRank because every type that derives
    /// or implements a trait funnels edges to these functions. Similarly,
    /// `field` nodes get high in-degree from parent struct `Defines` edges,
    /// and test-file symbols inflate importance for smoke/test infrastructure.
    ///
    /// This applies a multiplicative penalty (0.1) to these categories so
    /// they still appear in search results but don't crowd out real
    /// architectural hubs like handlers, builders, and core data structures.
    fn dampen_boilerplate(&self, scores: &mut HashMap<String, f64>) {
        /// Names of functions that are standard trait implementations.
        /// These are structurally connected (every type with the trait has one)
        /// but not architecturally significant.
        const BOILERPLATE_NAMES: &[&str] = &[
            "fmt", "default", "clone", "eq", "ne", "hash",
            "partial_cmp", "cmp", "from", "into", "drop",
            "deref", "deref_mut", "as_ref", "as_mut",
        ];

        /// Penalty multiplier for boilerplate nodes: 0.1 means they keep
        /// 10% of their raw importance score.
        const BOILERPLATE_PENALTY: f64 = 0.1;

        /// Test-file patterns in the file path component of stable IDs.
        const TEST_PATH_PATTERNS: &[&str] = &[
            "/tests/", "/test_", "_test.", "smoke.rs",
        ];

        for (stable_id, score) in scores.iter_mut() {
            if let Some(node_ref) = self.node_lookup.get(stable_id)
                .and_then(|&idx| self.graph.node_weight(idx))
            {
                // Parse the stable_id format: "root:file:name:kind"
                // We need the file and name components.
                let parts: Vec<&str> = stable_id.splitn(4, ':').collect();
                if parts.len() < 4 {
                    continue;
                }
                let file = parts[1];
                let name = parts[2];

                // 1. Dampen standard trait impl functions
                if node_ref.node_type == "function"
                    && BOILERPLATE_NAMES.contains(&name)
                {
                    *score *= BOILERPLATE_PENALTY;
                    continue;
                }

                // 2. Dampen field nodes
                if node_ref.node_type == "field" {
                    *score *= BOILERPLATE_PENALTY;
                    continue;
                }

                // 3. Dampen test-file symbols
                if TEST_PATH_PATTERNS.iter().any(|pat| file.contains(pat)) {
                    *score *= BOILERPLATE_PENALTY;
                    continue;
                }
            }
        }
    }
}

impl Default for GraphIndex {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Confidence, ExtractionSource, NodeId, NodeKind};
    use std::path::PathBuf;

    fn make_node_id(name: &str) -> NodeId {
        NodeId {
            root: "test".to_string(),
            file: PathBuf::from("src/lib.rs"),
            name: name.to_string(),
            kind: NodeKind::Function,
        }
    }

    fn make_edge(from: &str, to: &str, kind: EdgeKind) -> Edge {
        Edge {
            from: make_node_id(from),
            to: make_node_id(to),
            kind,
            source: ExtractionSource::TreeSitter,
            confidence: Confidence::Detected,
        }
    }

    #[test]
    fn test_add_node_and_edge() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "function", "b", "function", EdgeKind::Calls);

        assert_eq!(index.node_count(), 2);
        assert_eq!(index.edge_count(), 1);
    }

    #[test]
    fn test_ensure_node_idempotent() {
        let mut index = GraphIndex::new();
        let idx1 = index.ensure_node("a", "function");
        let idx2 = index.ensure_node("a", "function");
        assert_eq!(idx1, idx2);
        assert_eq!(index.node_count(), 1);
    }

    #[test]
    fn test_neighbors_outgoing() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "function", "b", "function", EdgeKind::Calls);
        index.add_edge("a", "function", "c", "struct", EdgeKind::DependsOn);
        index.add_edge("d", "function", "a", "function", EdgeKind::Calls);

        let out = index.neighbors("a", None, Direction::Outgoing);
        assert_eq!(out.len(), 2);
        assert!(out.contains(&"b".to_string()));
        assert!(out.contains(&"c".to_string()));
    }

    #[test]
    fn test_neighbors_incoming() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "function", "b", "function", EdgeKind::Calls);
        index.add_edge("c", "function", "b", "function", EdgeKind::Calls);

        let inc = index.neighbors("b", None, Direction::Incoming);
        assert_eq!(inc.len(), 2);
        assert!(inc.contains(&"a".to_string()));
        assert!(inc.contains(&"c".to_string()));
    }

    #[test]
    fn test_neighbors_filtered_by_edge_type() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "function", "b", "function", EdgeKind::Calls);
        index.add_edge("a", "function", "c", "struct", EdgeKind::DependsOn);

        let calls_only = index.neighbors("a", Some(&[EdgeKind::Calls]), Direction::Outgoing);
        assert_eq!(calls_only.len(), 1);
        assert_eq!(calls_only[0], "b");

        let deps_only = index.neighbors("a", Some(&[EdgeKind::DependsOn]), Direction::Outgoing);
        assert_eq!(deps_only.len(), 1);
        assert_eq!(deps_only[0], "c");
    }

    #[test]
    fn test_neighbors_nonexistent_node() {
        let index = GraphIndex::new();
        let result = index.neighbors("nonexistent", None, Direction::Outgoing);
        assert!(result.is_empty());
    }

    #[test]
    fn test_reachable_within_hops() {
        // a -> b -> c -> d
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "d", "fn", EdgeKind::Calls);

        let reach_1 = index.reachable("a", 1, None);
        assert_eq!(reach_1, vec!["b".to_string()]);

        let reach_2 = index.reachable("a", 2, None);
        assert_eq!(reach_2.len(), 2);
        assert!(reach_2.contains(&"b".to_string()));
        assert!(reach_2.contains(&"c".to_string()));

        let reach_3 = index.reachable("a", 3, None);
        assert_eq!(reach_3.len(), 3);
    }

    #[test]
    fn test_reachable_with_edge_filter() {
        // a -calls-> b -depends_on-> c -calls-> d
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::DependsOn);
        index.add_edge("c", "fn", "d", "fn", EdgeKind::Calls);

        // Only following Calls edges: a -> b, then b has no Calls outgoing
        let calls_only = index.reachable("a", 3, Some(&[EdgeKind::Calls]));
        assert_eq!(calls_only, vec!["b".to_string()]);
    }

    #[test]
    fn test_reachable_handles_cycles() {
        // a -> b -> c -> a (cycle)
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "a", "fn", EdgeKind::Calls);

        let reach = index.reachable("a", 10, None);
        // Should visit b and c but not loop forever
        assert_eq!(reach.len(), 2);
        assert!(reach.contains(&"b".to_string()));
        assert!(reach.contains(&"c".to_string()));
    }

    #[test]
    fn test_impact_reverse_bfs() {
        // a -> b -> c
        // d -> b
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("d", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);

        // Impact of c: who depends on c? b (directly), then a and d (transitively)
        let impact = index.impact("c", 10);
        assert_eq!(impact.len(), 3);
        assert!(impact.contains(&"b".to_string()));
        assert!(impact.contains(&"a".to_string()));
        assert!(impact.contains(&"d".to_string()));
    }

    #[test]
    fn test_impact_limited_hops() {
        // a -> b -> c -> d
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "d", "fn", EdgeKind::Calls);

        // Impact of d within 1 hop: only c
        let impact_1 = index.impact("d", 1);
        assert_eq!(impact_1, vec!["c".to_string()]);

        // Impact of d within 2 hops: c and b
        let impact_2 = index.impact("d", 2);
        assert_eq!(impact_2.len(), 2);
    }

    #[test]
    fn test_rebuild_from_edges() {
        let edges = vec![
            make_edge("foo", "bar", EdgeKind::Calls),
            make_edge("bar", "baz", EdgeKind::DependsOn),
        ];

        let mut index = GraphIndex::new();
        index.rebuild_from_edges(&edges);

        assert_eq!(index.node_count(), 3);
        assert_eq!(index.edge_count(), 2);

        // Verify the edges exist with correct types
        let foo_id = make_node_id("foo").to_stable_id();
        let bar_id = make_node_id("bar").to_stable_id();
        let baz_id = make_node_id("baz").to_stable_id();

        let foo_neighbors = index.neighbors(&foo_id, None, Direction::Outgoing);
        assert_eq!(foo_neighbors.len(), 1);
        assert_eq!(foo_neighbors[0], bar_id);

        let bar_neighbors = index.neighbors(&bar_id, None, Direction::Outgoing);
        assert_eq!(bar_neighbors.len(), 1);
        assert_eq!(bar_neighbors[0], baz_id);
    }

    #[test]
    fn test_rebuild_clears_previous() {
        let mut index = GraphIndex::new();
        index.add_edge("old_a", "fn", "old_b", "fn", EdgeKind::Calls);
        assert_eq!(index.node_count(), 2);

        // Rebuild with different edges
        let edges = vec![make_edge("new_x", "new_y", EdgeKind::Calls)];
        index.rebuild_from_edges(&edges);

        assert_eq!(index.node_count(), 2); // new_x and new_y
        assert!(index.get_node("old_a").is_none());
        assert!(index.get_node("old_b").is_none());
    }

    #[test]
    fn test_get_node() {
        let mut index = GraphIndex::new();
        index.ensure_node("my_func", "function");

        let node = index.get_node("my_func");
        assert!(node.is_some());
        assert_eq!(node.unwrap().id, "my_func");
        assert_eq!(node.unwrap().node_type, "function");

        assert!(index.get_node("nonexistent").is_none());
    }

    // ── Multi-entry traversal tests (PR #113 semantic graph entry) ──────

    /// Helper: simulate the multi-entry traversal + dedup pattern from the
    /// graph_query handler. Returns (all_ids_deduped, entry_nodes_stripped).
    fn multi_entry_neighbors(
        index: &GraphIndex,
        entry_ids: &[&str],
        edge_filter: Option<&[EdgeKind]>,
        direction: Direction,
    ) -> Vec<String> {
        let mut all_ids: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for node_id in entry_ids {
            let ids = index.neighbors(node_id, edge_filter, direction);
            for id in ids {
                if seen.insert(id.clone()) {
                    all_ids.push(id);
                }
            }
        }

        // Strip entry nodes from results (matches handler behavior)
        let entry_set: std::collections::HashSet<&str> =
            entry_ids.iter().copied().collect();
        all_ids.retain(|id| !entry_set.contains(id.as_str()));
        all_ids
    }

    #[test]
    fn test_multi_entry_deduplicates_shared_neighbors() {
        // a -> c, b -> c  (both entry nodes point to same neighbor)
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);

        let result = multi_entry_neighbors(&index, &["a", "b"], None, Direction::Outgoing);
        // c should appear exactly once, not twice
        assert_eq!(result, vec!["c".to_string()]);
    }

    #[test]
    fn test_multi_entry_strips_entry_nodes_from_results() {
        // a -> b -> c, where b is also an entry node
        // If entry nodes are {a, b}, then b's outgoing neighbor c should appear
        // but a and b themselves should be stripped from the result
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);

        let result = multi_entry_neighbors(&index, &["a", "b"], None, Direction::Outgoing);
        // a's neighbor is b (stripped as entry), b's neighbor is c (kept)
        assert_eq!(result, vec!["c".to_string()]);
        assert!(!result.contains(&"a".to_string()));
        assert!(!result.contains(&"b".to_string()));
    }

    #[test]
    fn test_multi_entry_entry_node_is_neighbor_of_another_entry() {
        // a -> b, b -> c   (entry nodes: a, b)
        // b is both an entry node AND a neighbor of entry node a
        // The handler strips entry nodes, so we lose the a->b edge info
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("a", "fn", "d", "fn", EdgeKind::Calls);

        let result = multi_entry_neighbors(&index, &["a", "b"], None, Direction::Outgoing);
        // b is stripped (entry node), even though it's a valid neighbor of a
        // This is the behavior bug noted in review finding #5
        assert!(!result.contains(&"b".to_string()));
        assert!(result.contains(&"c".to_string()));
        assert!(result.contains(&"d".to_string()));
    }

    #[test]
    fn test_multi_entry_all_neighbors_are_entry_nodes() {
        // a -> b, b -> a   (entry nodes: a, b, cyclic)
        // All neighbors are entry nodes — result should be empty
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "a", "fn", EdgeKind::Calls);

        let result = multi_entry_neighbors(&index, &["a", "b"], None, Direction::Outgoing);
        // Both a and b are entry nodes, so everything gets stripped
        assert!(result.is_empty());
    }

    #[test]
    fn test_multi_entry_single_entry_same_as_old_behavior() {
        // a -> b, a -> c (single entry node: equivalent to old non-multi code path)
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("a", "fn", "c", "struct", EdgeKind::DependsOn);

        let multi = multi_entry_neighbors(&index, &["a"], None, Direction::Outgoing);
        let single = index.neighbors("a", None, Direction::Outgoing);
        // Single entry should give same results (no stripping since entry != neighbor)
        assert_eq!(multi.len(), single.len());
        for id in &single {
            assert!(multi.contains(id));
        }
    }

    #[test]
    fn test_multi_entry_nonexistent_entry_nodes_produce_empty() {
        let index = GraphIndex::new();
        // No nodes in graph at all
        let result = multi_entry_neighbors(&index, &["ghost1", "ghost2"], None, Direction::Outgoing);
        assert!(result.is_empty());
    }

    #[test]
    fn test_multi_entry_preserves_insertion_order() {
        // a -> x, a -> y, b -> z
        // Order should be: x, y (from a), z (from b) — insertion order preserved
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "x", "fn", EdgeKind::Calls);
        index.add_edge("a", "fn", "y", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "z", "fn", EdgeKind::Calls);

        let result = multi_entry_neighbors(&index, &["a", "b"], None, Direction::Outgoing);
        assert_eq!(result.len(), 3);
        // z should be last since it comes from the second entry node
        assert_eq!(result[2], "z".to_string());
    }

    // ── PageRank tests ──────────────────────────────────────────────────

    #[test]
    fn test_pagerank_empty_graph() {
        let index = GraphIndex::new();
        let scores = index.compute_pagerank(0.85, 20);
        assert!(scores.is_empty());
    }

    #[test]
    fn test_pagerank_single_node_no_edges() {
        let mut index = GraphIndex::new();
        index.ensure_node("a", "function");
        let scores = index.compute_pagerank(0.85, 20);
        assert_eq!(scores.len(), 1);
        // Single node gets max score = 1.0 after normalization
        assert!((scores["a"] - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_pagerank_linear_chain() {
        // a -> b -> c: c is the sink, should have highest score
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);

        let scores = index.compute_pagerank(0.85, 20);
        assert_eq!(scores.len(), 3);
        // c receives more incoming flow than a
        assert!(scores["c"] > scores["a"], "sink should rank higher than source");
    }

    #[test]
    fn test_pagerank_hub_vs_leaf() {
        // Hub pattern: a -> d, b -> d, c -> d (d is the hub)
        // Leaf: e -> f (isolated pair)
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "d", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "d", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "d", "fn", EdgeKind::Calls);
        index.add_edge("e", "fn", "f", "fn", EdgeKind::Calls);

        let scores = index.compute_pagerank(0.85, 20);
        // d should have higher importance than f (3 callers vs 1 caller)
        assert!(scores["d"] > scores["f"], "hub should rank higher than leaf");
    }

    #[test]
    fn test_pagerank_scores_normalized() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "a", "fn", EdgeKind::Calls);

        let scores = index.compute_pagerank(0.85, 20);
        // Scores should be in [0, 1] range
        for &score in scores.values() {
            assert!(score >= 0.0 && score <= 1.0, "score {} out of [0,1] range", score);
        }
        // At least one node should have max score of 1.0
        let max = scores.values().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!((max - 1.0).abs() < 0.01, "max score should be ~1.0, got {}", max);
    }

    // ==================== Adversarial PageRank tests ====================
    // Seeded from dissent: disconnected components, near-uniform graphs, NaN safety.

    /// Dissent finding: disconnected components may produce 0 or NaN scores.
    /// Two disconnected subgraphs should both have scores in [0, 1] with
    /// at least one node at 1.0 (the global max).
    #[test]
    fn test_pagerank_disconnected_components() {
        let mut index = GraphIndex::new();
        // Component 1: a -> b -> c
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        // Component 2: x -> y (completely disconnected)
        index.add_edge("x", "fn", "y", "fn", EdgeKind::Calls);

        let scores = index.compute_pagerank(0.85, 20);
        assert_eq!(scores.len(), 5, "all 5 nodes should have scores");
        for (id, &score) in &scores {
            assert!(score >= 0.0 && score <= 1.0,
                "node {} has out-of-range score {}", id, score);
            assert!(!score.is_nan(), "node {} has NaN score", id);
        }
        let max = scores.values().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!((max - 1.0).abs() < 0.01, "max score should be ~1.0, got {}", max);
    }

    /// Dissent finding: near-uniform distribution on small symmetric graphs.
    /// A 3-node clique where every node calls every other — scores should
    /// be near-equal after max-normalization (all ~1.0). This isn't a bug,
    /// it's expected behavior: all nodes are equally important in a clique.
    #[test]
    fn test_pagerank_small_clique_near_uniform() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("a", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "a", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "a", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "b", "fn", EdgeKind::Calls);

        let scores = index.compute_pagerank(0.85, 20);
        // In a complete clique, all nodes should have similar scores
        let vals: Vec<f64> = scores.values().copied().collect();
        let min = vals.iter().copied().fold(f64::INFINITY, f64::min);
        let max = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        // After max-normalization, max is 1.0 and min should be very close
        assert!((max - 1.0).abs() < 0.01);
        assert!(min > 0.9, "in a clique, min should be near max, got {}", min);
    }

    /// Dissent finding: 20 iterations may not converge on deep chains.
    /// A chain of 50 nodes: a0 -> a1 -> ... -> a49. The sink (a49) should
    /// have the highest score, and all scores should be valid.
    #[test]
    fn test_pagerank_deep_chain_convergence() {
        let mut index = GraphIndex::new();
        for i in 0..49 {
            index.add_edge(
                &format!("a{}", i), "fn",
                &format!("a{}", i + 1), "fn",
                EdgeKind::Calls,
            );
        }

        let scores = index.compute_pagerank(0.85, 20);
        assert_eq!(scores.len(), 50);

        // All scores valid
        for (id, &score) in &scores {
            assert!(score >= 0.0 && score <= 1.0 && !score.is_nan(),
                "node {} has invalid score {}", id, score);
        }

        // Sink (a49) should have the highest score
        let sink_score = scores[&format!("a49")];
        let source_score = scores[&format!("a0")];
        assert!(sink_score > source_score,
            "sink should rank higher than source: {} vs {}", sink_score, source_score);
    }

    // ==================== Boilerplate dampening tests ====================

    /// Helper: build a stable_id in the format "root:file:name:kind"
    fn stable_id(file: &str, name: &str, kind: &str) -> String {
        format!("test:{}:{}:{}", file, name, kind)
    }

    #[test]
    fn test_dampen_boilerplate_fmt_function() {
        let mut index = GraphIndex::new();
        let fmt_id = stable_id("src/types.rs", "fmt", "function");
        let handler_id = stable_id("src/server.rs", "handle_request", "function");
        let caller1 = stable_id("src/a.rs", "caller1", "function");
        let caller2 = stable_id("src/b.rs", "caller2", "function");

        // Both fmt and handle_request are called by 2 callers (same structure)
        index.add_edge(&caller1, "function", &fmt_id, "function", EdgeKind::Calls);
        index.add_edge(&caller2, "function", &fmt_id, "function", EdgeKind::Calls);
        index.add_edge(&caller1, "function", &handler_id, "function", EdgeKind::Calls);
        index.add_edge(&caller2, "function", &handler_id, "function", EdgeKind::Calls);

        let scores = index.compute_pagerank(0.85, 20);

        assert!(scores[&fmt_id] < scores[&handler_id],
            "fmt ({:.4}) should score lower than handle_request ({:.4})",
            scores[&fmt_id], scores[&handler_id]);
        assert!(scores[&fmt_id] < scores[&handler_id] * 0.2,
            "fmt should be significantly dampened");
    }

    #[test]
    fn test_dampen_boilerplate_default_and_clone() {
        let mut index = GraphIndex::new();
        let default_id = stable_id("src/types.rs", "default", "function");
        let clone_id = stable_id("src/types.rs", "clone", "function");
        let build_id = stable_id("src/build.rs", "build_graph", "function");
        let caller = stable_id("src/main.rs", "main", "function");

        index.add_edge(&caller, "function", &default_id, "function", EdgeKind::Calls);
        index.add_edge(&caller, "function", &clone_id, "function", EdgeKind::Calls);
        index.add_edge(&caller, "function", &build_id, "function", EdgeKind::Calls);

        let scores = index.compute_pagerank(0.85, 20);

        assert!(scores[&default_id] < scores[&build_id],
            "default should score lower than build_graph");
        assert!(scores[&clone_id] < scores[&build_id],
            "clone should score lower than build_graph");
    }

    #[test]
    fn test_dampen_boilerplate_field_nodes() {
        let mut index = GraphIndex::new();
        let field_id = stable_id("src/types.rs", "name", "field");
        let struct_id = stable_id("src/types.rs", "Config", "struct");
        let func_id = stable_id("src/lib.rs", "process", "function");

        index.add_edge(&struct_id, "struct", &field_id, "field", EdgeKind::HasField);
        index.add_edge(&func_id, "function", &struct_id, "struct", EdgeKind::DependsOn);
        let caller = stable_id("src/a.rs", "reader", "function");
        index.add_edge(&caller, "function", &field_id, "field", EdgeKind::Calls);

        let scores = index.compute_pagerank(0.85, 20);

        assert!(scores[&field_id] < scores[&struct_id],
            "field node ({:.4}) should score lower than struct ({:.4})",
            scores[&field_id], scores[&struct_id]);
    }

    #[test]
    fn test_dampen_boilerplate_test_file_symbols() {
        let mut index = GraphIndex::new();
        let test_fn = stable_id("src/smoke.rs", "run_checks", "function");
        let prod_fn = stable_id("src/server.rs", "run_server", "function");
        let caller = stable_id("src/main.rs", "main", "function");

        index.add_edge(&caller, "function", &test_fn, "function", EdgeKind::Calls);
        index.add_edge(&caller, "function", &prod_fn, "function", EdgeKind::Calls);

        let scores = index.compute_pagerank(0.85, 20);

        assert!(scores[&test_fn] < scores[&prod_fn],
            "smoke.rs function ({:.4}) should score lower than server.rs function ({:.4})",
            scores[&test_fn], scores[&prod_fn]);
    }

    #[test]
    fn test_dampen_preserves_non_boilerplate_scores() {
        let mut index = GraphIndex::new();
        let func = stable_id("src/query.rs", "search_all", "function");
        let strct = stable_id("src/graph.rs", "GraphIndex", "struct");
        let caller = stable_id("src/main.rs", "main", "function");

        index.add_edge(&caller, "function", &func, "function", EdgeKind::Calls);
        index.add_edge(&caller, "function", &strct, "struct", EdgeKind::DependsOn);

        let scores = index.compute_pagerank(0.85, 20);

        let diff = (scores[&func] - scores[&strct]).abs();
        assert!(diff < 0.1,
            "non-boilerplate symbols should have similar scores: {} vs {}",
            scores[&func], scores[&strct]);
    }

    #[test]
    fn test_dampen_existing_tests_unaffected() {
        // Simple IDs without root:file:name:kind format are not dampened
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);

        let scores = index.compute_pagerank(0.85, 20);

        assert!((scores["b"] - 1.0).abs() < 0.01,
            "simple IDs should not be dampened, got {}", scores["b"]);
    }
}
