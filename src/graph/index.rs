//! In-memory petgraph index for structural graph traversal.
//!
//! The `GraphIndex` is a derived index rebuilt from LanceDB edge data.
//! It provides fast BFS/DFS traversal, neighbor queries, and impact
//! analysis that would be expensive as columnar joins.

use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef as PetgraphEdgeRef;
use petgraph::Direction;

use super::{Edge, EdgeKind};

// ---------------------------------------------------------------------------
// Edge-type weights for PageRank
// ---------------------------------------------------------------------------

/// Returns the PageRank transition weight for a given edge type.
///
/// Higher weights mean the edge carries more "importance" signal during
/// the random walk. This is language-agnostic: weights are based on edge
/// semantics (coupling strength), not symbol names or language conventions.
fn edge_weight(kind: &EdgeKind) -> f64 {
    match kind {
        EdgeKind::Calls => 1.0,
        EdgeKind::Implements => 0.8,
        EdgeKind::DependsOn => 0.5,
        EdgeKind::ReferencedBy => 0.5,
        EdgeKind::ConnectsTo => 0.3,
        EdgeKind::Defines => 0.1,
        EdgeKind::HasField => 0.1,
        // PR/outcome edges carry no architectural coupling signal
        EdgeKind::Evolves
        | EdgeKind::TopologyBoundary
        | EdgeKind::Modified
        | EdgeKind::Affected
        | EdgeKind::Serves => 0.05,
    }
}

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
#[derive(Clone)]
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
    ///
    /// When `edge_types` is `Some`, only edges matching those types are
    /// traversed. This is important for impact analysis: metadata edges
    /// like `Modified` (PrMerge -> symbol) and `Serves` should be excluded
    /// so the traversal stays within the code dependency graph.
    pub fn impact(
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

            for edge_ref in self.graph.edges_directed(current, Direction::Incoming) {
                if let Some(types) = edge_types {
                    if !types.contains(&edge_ref.weight().edge_type) {
                        continue;
                    }
                }

                let neighbor = edge_ref.source();
                if visited.insert(neighbor) {
                    result.push(self.graph[neighbor].id.clone());
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }

        result
    }

    /// Compute edge-type weighted PageRank importance scores.
    ///
    /// Unlike standard PageRank (which treats all edges equally), this
    /// attenuates rank flow by edge type so that architectural coupling
    /// signals (`Calls`, `Implements`) transfer more importance than
    /// structural containment (`Defines`, `HasField`).
    ///
    /// The key insight: edge weight acts as an **attenuation factor**, not
    /// just a proportional distribution. A `Defines` edge (weight 0.1)
    /// only transmits 10% of the source's rank share to the target; the
    /// other 90% is "lost" to teleportation. This means a node reachable
    /// only via `Defines` edges accumulates far less importance than one
    /// reachable via `Calls` edges, even with the same topology.
    ///
    /// This approach is fully language-agnostic: weights come from edge
    /// semantics, not symbol names. No per-language blocklists needed.
    ///
    /// Edge weights (see [`edge_weight`]):
    /// - `Calls` = 1.0, `Implements` = 0.8, `DependsOn` = 0.5
    /// - `Defines` = 0.1, `HasField` = 0.1
    ///
    /// Scores are max-normalized to [0, 1] where 1.0 is the most important
    /// node. These are relative ranks, not probabilities.
    pub fn compute_pagerank(&self, damping_factor: f64, nb_iter: usize) -> HashMap<String, f64> {
        let n = self.graph.node_count();
        if n == 0 {
            return HashMap::new();
        }

        let init = 1.0 / n as f64;
        let mut scores = vec![init; n];
        let mut new_scores = vec![0.0_f64; n];

        for _ in 0..nb_iter {
            // Reset new scores to the teleportation term
            for s in new_scores.iter_mut() {
                *s = (1.0 - damping_factor) / n as f64;
            }

            // For each node, distribute its score to neighbors.
            // Edge weight attenuates the flow: a Defines edge (0.1) only
            // passes 10% of the per-edge share, with the rest going to
            // teleportation (uniform redistribution).
            for node_idx in self.graph.node_indices() {
                let idx = node_idx.index();
                let current_score = scores[idx];

                let out_edges: Vec<_> = self.graph.edges(node_idx).collect();
                let out_degree = out_edges.len() as f64;

                if out_degree > 0.0 {
                    // Each edge gets an equal share of rank (1/out_degree),
                    // then attenuated by the edge weight. The un-transferred
                    // portion is implicitly absorbed into teleportation.
                    let per_edge = current_score / out_degree;
                    for e in &out_edges {
                        let w = edge_weight(&e.weight().edge_type);
                        let target = e.target().index();
                        new_scores[target] += damping_factor * per_edge * w;
                    }
                } else {
                    // Dangling node: distribute evenly (standard PageRank behavior)
                    let share = damping_factor * current_score / n as f64;
                    for s in new_scores.iter_mut() {
                        *s += share;
                    }
                }
            }

            std::mem::swap(&mut scores, &mut new_scores);
        }

        // Max-normalize to [0, 1]
        let max_score = scores
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);

        let mut result = HashMap::new();
        if max_score <= 0.0 {
            return result;
        }

        for node_idx in self.graph.node_indices() {
            if let Some(node_ref) = self.graph.node_weight(node_idx) {
                result.insert(node_ref.id.clone(), scores[node_idx.index()] / max_score);
            }
        }
        result
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
        let impact = index.impact("c", 10, None);
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
        let impact_1 = index.impact("d", 1, None);
        assert_eq!(impact_1, vec!["c".to_string()]);

        // Impact of d within 2 hops: c and b
        let impact_2 = index.impact("d", 2, None);
        assert_eq!(impact_2.len(), 2);
    }

    #[test]
    fn test_impact_filtered_by_edge_type() {
        // a -calls-> b, pr_merge -modified-> b
        // Impact of b without filter: a and pr_merge
        // Impact of b with Calls filter: only a
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("pr_merge_1", "pr_merge", "b", "fn", EdgeKind::Modified);

        let impact_all = index.impact("b", 10, None);
        assert_eq!(impact_all.len(), 2);
        assert!(impact_all.contains(&"a".to_string()));
        assert!(impact_all.contains(&"pr_merge_1".to_string()));

        let impact_calls_only = index.impact("b", 10, Some(&[EdgeKind::Calls]));
        assert_eq!(impact_calls_only.len(), 1);
        assert_eq!(impact_calls_only[0], "a");
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

    // ==================== Edge-type weighted PageRank tests ====================
    // Validates that edge-type weights produce correct importance ranking:
    // Calls > Implements > DependsOn > Defines/HasField.

    /// Core test: a node reachable via Calls edges should rank higher than
    /// a node reachable only via Defines edges, even with the same topology.
    #[test]
    fn test_weighted_pagerank_calls_beats_defines() {
        let mut index = GraphIndex::new();
        // "hub_calls" receives 3 Calls edges (weight 1.0 each)
        index.add_edge("a", "fn", "hub_calls", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "hub_calls", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "hub_calls", "fn", EdgeKind::Calls);
        // "hub_defines" receives 3 Defines edges (weight 0.1 each)
        index.add_edge("d", "struct", "hub_defines", "field", EdgeKind::Defines);
        index.add_edge("e", "struct", "hub_defines", "field", EdgeKind::Defines);
        index.add_edge("f", "struct", "hub_defines", "field", EdgeKind::Defines);

        let scores = index.compute_pagerank(0.85, 20);
        assert!(
            scores["hub_calls"] > scores["hub_defines"],
            "Calls hub ({:.4}) should rank higher than Defines hub ({:.4})",
            scores["hub_calls"],
            scores["hub_defines"]
        );
    }

    /// Implements edges (weight 0.8) should produce higher scores than
    /// HasField edges (weight 0.1) for otherwise identical topology.
    #[test]
    fn test_weighted_pagerank_implements_beats_has_field() {
        let mut index = GraphIndex::new();
        // "trait_hub" receives 3 Implements edges
        index.add_edge("impl_a", "impl", "trait_hub", "trait", EdgeKind::Implements);
        index.add_edge("impl_b", "impl", "trait_hub", "trait", EdgeKind::Implements);
        index.add_edge("impl_c", "impl", "trait_hub", "trait", EdgeKind::Implements);
        // "struct_hub" receives 3 HasField edges
        index.add_edge("struct_a", "struct", "struct_hub", "field", EdgeKind::HasField);
        index.add_edge("struct_b", "struct", "struct_hub", "field", EdgeKind::HasField);
        index.add_edge("struct_c", "struct", "struct_hub", "field", EdgeKind::HasField);

        let scores = index.compute_pagerank(0.85, 20);
        assert!(
            scores["trait_hub"] > scores["struct_hub"],
            "Implements hub ({:.4}) should rank higher than HasField hub ({:.4})",
            scores["trait_hub"],
            scores["struct_hub"]
        );
    }

    /// Mixed edge types: a node with 1 Calls edge should outrank a node
    /// with 5 Defines edges. Quality of edges beats quantity.
    #[test]
    fn test_weighted_pagerank_one_call_beats_many_defines() {
        let mut index = GraphIndex::new();
        // "called_once" gets 1 Calls edge
        index.add_edge("caller", "fn", "called_once", "fn", EdgeKind::Calls);
        // "defined_many" gets 5 Defines edges
        for i in 0..5 {
            index.add_edge(
                &format!("definer_{}", i), "struct",
                "defined_many", "field",
                EdgeKind::Defines,
            );
        }

        let scores = index.compute_pagerank(0.85, 20);
        assert!(
            scores["called_once"] > scores["defined_many"],
            "1 Calls edge ({:.4}) should outrank 5 Defines edges ({:.4})",
            scores["called_once"],
            scores["defined_many"]
        );
    }

    /// Verify the edge_weight function returns expected values for all edge kinds.
    #[test]
    fn test_edge_weight_values() {
        use super::edge_weight;
        assert!((edge_weight(&EdgeKind::Calls) - 1.0).abs() < f64::EPSILON);
        assert!((edge_weight(&EdgeKind::Implements) - 0.8).abs() < f64::EPSILON);
        assert!((edge_weight(&EdgeKind::DependsOn) - 0.5).abs() < f64::EPSILON);
        assert!((edge_weight(&EdgeKind::ReferencedBy) - 0.5).abs() < f64::EPSILON);
        assert!((edge_weight(&EdgeKind::ConnectsTo) - 0.3).abs() < f64::EPSILON);
        assert!((edge_weight(&EdgeKind::Defines) - 0.1).abs() < f64::EPSILON);
        assert!((edge_weight(&EdgeKind::HasField) - 0.1).abs() < f64::EPSILON);
        // PR/outcome edges get minimal weight
        assert!(edge_weight(&EdgeKind::Modified) < 0.1);
        assert!(edge_weight(&EdgeKind::Serves) < 0.1);
    }
}
