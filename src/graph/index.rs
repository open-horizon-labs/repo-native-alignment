//! In-memory petgraph index for structural graph traversal.
//!
//! The `GraphIndex` is a derived index rebuilt from LanceDB edge data.
//! It provides fast BFS/DFS traversal, neighbor queries, and impact
//! analysis that would be expensive as columnar joins.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use petgraph::algo::{dijkstra, tarjan_scc};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef as PetgraphEdgeRef;
use petgraph::Direction;

use super::{Edge, EdgeKind};
use crate::ranking::is_test_path;

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
        EdgeKind::References => 0.5,
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

    /// 1-hop neighbors grouped by edge type.
    ///
    /// Returns a `BTreeMap<EdgeKind, Vec<String>>` where each key is an edge type
    /// and the value is the list of neighbor IDs connected by that edge type.
    /// Edge types with no neighbors are omitted. BTreeMap ensures consistent
    /// ordering across calls.
    pub fn neighbors_grouped(
        &self,
        node_id: &str,
        edge_types: Option<&[EdgeKind]>,
        direction: Direction,
    ) -> BTreeMap<EdgeKind, Vec<String>> {
        let Some(&idx) = self.node_lookup.get(node_id) else {
            return BTreeMap::new();
        };

        let mut groups: BTreeMap<EdgeKind, Vec<String>> = BTreeMap::new();
        let edges = self.graph.edges_directed(idx, direction);

        for edge_ref in edges {
            let kind = &edge_ref.weight().edge_type;
            if let Some(types) = edge_types {
                if !types.contains(kind) {
                    continue;
                }
            }

            let neighbor_idx = match direction {
                Direction::Outgoing => edge_ref.target(),
                Direction::Incoming => edge_ref.source(),
            };
            groups
                .entry(kind.clone())
                .or_default()
                .push(self.graph[neighbor_idx].id.clone());
        }

        groups
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

    /// Detect strongly connected components (SCCs) in the coupling subgraph.
    ///
    /// Runs Tarjan's SCC algorithm on edges of the given types (defaults to
    /// `Calls` and `DependsOn`). Returns only SCCs with more than one node —
    /// those are circular dependency rings.
    ///
    /// Each inner `Vec<String>` is one ring, with node IDs in reverse
    /// topological order (Tarjan's natural output).
    ///
    /// # Example
    /// ```
    /// // a -> b -> c -> a  (ring)
    /// // x -> y            (no ring)
    /// let rings = index.detect_cycles(None);
    /// assert_eq!(rings.len(), 1);
    /// assert_eq!(rings[0].len(), 3);
    /// ```
    pub fn detect_cycles(&self, edge_types: Option<&[EdgeKind]>) -> Vec<Vec<String>> {
        let coupling_defaults = [EdgeKind::Calls, EdgeKind::DependsOn];
        let filter = edge_types.unwrap_or(&coupling_defaults);

        // Build a filtered subgraph containing only the requested edge types.
        // petgraph's tarjan_scc operates on the graph directly, so we build a
        // fresh DiGraph with only the filtered edges rather than mutating self.
        let mut sub: DiGraph<String, ()> = DiGraph::new();
        let mut sub_lookup: HashMap<String, NodeIndex> = HashMap::new();

        let ensure_sub_node = |g: &mut DiGraph<String, ()>,
                                lk: &mut HashMap<String, NodeIndex>,
                                id: &str|
         -> NodeIndex {
            if let Some(&idx) = lk.get(id) {
                idx
            } else {
                let idx = g.add_node(id.to_string());
                lk.insert(id.to_string(), idx);
                idx
            }
        };

        for edge_ref in self.graph.edge_references() {
            if !filter.contains(&edge_ref.weight().edge_type) {
                continue;
            }
            let src_id = &self.graph[edge_ref.source()].id;
            let tgt_id = &self.graph[edge_ref.target()].id;
            let s = ensure_sub_node(&mut sub, &mut sub_lookup, src_id);
            let t = ensure_sub_node(&mut sub, &mut sub_lookup, tgt_id);
            sub.add_edge(s, t, ());
        }

        if sub.node_count() == 0 {
            return Vec::new();
        }

        // tarjan_scc returns all SCCs (including size-1 trivial ones).
        // Keep only rings (size > 1).
        tarjan_scc(&sub)
            .into_iter()
            .filter(|scc| scc.len() > 1)
            .map(|scc| scc.into_iter().map(|idx| sub[idx].clone()).collect())
            .collect()
    }

    /// Return the SCC ring that contains `node_id`, if any.
    ///
    /// Returns `None` if the node has no cycle membership.
    pub fn cycle_for_node(&self, node_id: &str, edge_types: Option<&[EdgeKind]>) -> Option<Vec<String>> {
        self.detect_cycles(edge_types)
            .into_iter()
            .find(|ring| ring.contains(&node_id.to_string()))
    }

    /// Compute the shortest directed path between two nodes via Dijkstra.
    ///
    /// Traverses `Calls` edges by default; pass `edge_types` to restrict to
    /// other edge kinds. Returns the path as an ordered list of node IDs
    /// from `from_id` (exclusive) to `to_id` (inclusive), or `None` if no
    /// path exists.
    ///
    /// The `from_id` node is not included in the result so callers can
    /// format it as `"A → B → C"` without duplicating the start node.
    ///
    /// # Example
    /// ```
    /// // a -> b -> c
    /// let path = index.shortest_path("a", "c", None);
    /// assert_eq!(path, Some(vec!["b".to_string(), "c".to_string()]));
    /// ```
    pub fn shortest_path(
        &self,
        from_id: &str,
        to_id: &str,
        edge_types: Option<&[EdgeKind]>,
    ) -> Option<Vec<String>> {
        let calls_default = [EdgeKind::Calls];
        let filter = edge_types.unwrap_or(&calls_default);

        let Some(&from_idx) = self.node_lookup.get(from_id) else {
            return None;
        };
        let Some(&to_idx) = self.node_lookup.get(to_id) else {
            return None;
        };

        if from_idx == to_idx {
            return Some(Vec::new());
        }

        // Build filtered subgraph identical to detect_cycles approach.
        // Map original NodeIndex -> sub NodeIndex so we can reconstruct the path.
        let mut sub: DiGraph<String, ()> = DiGraph::new();
        let mut orig_to_sub: HashMap<NodeIndex, NodeIndex> = HashMap::new();

        let ensure_sub = |g: &mut DiGraph<String, ()>,
                           map: &mut HashMap<NodeIndex, NodeIndex>,
                           orig: NodeIndex,
                           label: &str|
         -> NodeIndex {
            if let Some(&s) = map.get(&orig) {
                s
            } else {
                let s = g.add_node(label.to_string());
                map.insert(orig, s);
                s
            }
        };

        for edge_ref in self.graph.edge_references() {
            if !filter.contains(&edge_ref.weight().edge_type) {
                continue;
            }
            let src_orig = edge_ref.source();
            let tgt_orig = edge_ref.target();
            let src_label = &self.graph[src_orig].id;
            let tgt_label = &self.graph[tgt_orig].id;
            let s = ensure_sub(&mut sub, &mut orig_to_sub, src_orig, src_label);
            let t = ensure_sub(&mut sub, &mut orig_to_sub, tgt_orig, tgt_label);
            sub.add_edge(s, t, ());
        }

        let sub_from = *orig_to_sub.get(&from_idx)?;
        let sub_to   = *orig_to_sub.get(&to_idx)?;

        // Dijkstra with uniform edge weight 1. Returns distances from sub_from.
        let distances = dijkstra(&sub, sub_from, Some(sub_to), |_| 1u32);

        if !distances.contains_key(&sub_to) {
            return None; // no path
        }

        // Reconstruct path by greedy predecessor walk: at each step, find a
        // neighbour whose distance == current_dist - 1.
        let mut path_nodes: Vec<NodeIndex> = Vec::new();
        let mut current = sub_to;
        loop {
            path_nodes.push(current);
            if current == sub_from {
                break;
            }
            let cur_dist = distances[&current];
            // Walk incoming edges to find a predecessor one hop closer.
            let prev = sub
                .edges_directed(current, Direction::Incoming)
                .find(|e| distances.get(&e.source()).copied() == Some(cur_dist - 1))
                .map(|e| e.source())?;
            current = prev;
        }

        path_nodes.reverse();
        // Return the path excluding the start node (from_id).
        let result: Vec<String> = path_nodes
            .into_iter()
            .skip(1)
            .map(|idx| sub[idx].clone())
            .collect();
        Some(result)
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


    /// Detect subsystems via Louvain community detection on coupling edges.
    ///
    /// Only coupling edges are considered: Calls (1.0), Implements (0.8),
    /// DependsOn (0.5), ReferencedBy (0.5), References (0.5), ConnectsTo (0.3).
    /// Defines and HasField are excluded because they make every directory a
    /// trivial cluster (structural containment, not coupling).
    ///
    /// Returns a list of detected subsystems, each with:
    /// - A name derived from the dominant file-path prefix of member nodes
    /// - Symbol count and cohesion ratio (internal edges / total edges)
    /// - Interface functions scored by cross_cluster_degree * pagerank
    ///
    /// The `pagerank_scores` parameter should come from `compute_pagerank()`.
    /// The `node_file_map` maps node IDs to their file paths (for cluster naming).
    pub fn detect_communities(
        &self,
        pagerank_scores: &HashMap<String, f64>,
        node_file_map: &HashMap<String, String>,
    ) -> Vec<Subsystem> {
        // Step 0: Identify test nodes. Test functions form small tight clusters
        // that are not architectural units. We exclude them from Louvain and
        // assign them to their nearest production subsystem post-hoc.
        let n = self.graph.node_count();
        if n == 0 {
            return Vec::new();
        }

        let is_test_node: Vec<bool> = (0..n)
            .map(|idx| {
                let nr = &self.graph[NodeIndex::new(idx)];
                node_file_map
                    .get(&nr.id)
                    .map(|p| is_test_path(p))
                    .unwrap_or(false)
            })
            .collect();

        // Step 1: Build coupling subgraph as an undirected adjacency list.
        // Edge selection: only edges that represent actual coupling between
        // symbols. DependsOn (imports) is excluded because it creates trivially
        // large clusters -- every file that imports a common type ends up in one
        // giant community. Calls and Implements are the strongest coupling signals.
        let coupling_kinds = [
            EdgeKind::Calls,
            EdgeKind::Implements,
            EdgeKind::ReferencedBy,
            EdgeKind::References,
            EdgeKind::ConnectsTo,
        ];

        // adj[node_index] = vec of (neighbor_index, weight)
        // Only production (non-test) nodes participate.
        let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        let mut total_weight = 0.0;

        for edge_ref in self.graph.edge_references() {
            let kind = &edge_ref.weight().edge_type;
            if !coupling_kinds.contains(kind) {
                continue;
            }
            let s = edge_ref.source().index();
            let t = edge_ref.target().index();
            if s == t || is_test_node[s] || is_test_node[t] {
                continue;
            }
            let w = edge_weight(kind);
            adj[s].push((t, w));
            adj[t].push((s, w));
            total_weight += w;
        }

        if total_weight == 0.0 {
            return Vec::new();
        }

        // Guard: require a minimum density of coupling edges relative to node
        // count. Without enough edges (e.g., no LSP data), the graph is too
        // sparse for Louvain to find meaningful communities.
        let prod_count = is_test_node.iter().filter(|t| !**t).count();
        let nodes_with_edges = adj.iter().filter(|nbrs| !nbrs.is_empty()).count();
        if nodes_with_edges < 3
            || (prod_count > 100 && (nodes_with_edges as f64) < (prod_count as f64 * 0.05))
        {
            return Vec::new();
        }

        // Resolution parameter gamma: lower values produce fewer, larger clusters.
        // gamma=1.0 is standard Louvain; gamma<1.0 penalizes small communities.
        let gamma: f64 = 0.8;

        // Step 2: Louvain Phase 1 -- iteratively move nodes to maximize modularity.
        let community = louvain_phase1(&adj, total_weight, gamma, n);

        // Step 3: Louvain Phase 2 -- contract communities into super-nodes and
        // repeat Phase 1 on the coarsened graph until no further improvement.
        let mut community = louvain_phase2(&adj, community, gamma, n);

        // Step 4: Collect communities, compute stats, and build Subsystem structs.
        // Group production nodes by community
        let mut comm_members: HashMap<usize, Vec<usize>> = HashMap::new();
        for (node_idx, &comm) in community.iter().enumerate() {
            if !is_test_node[node_idx] {
                comm_members.entry(comm).or_default().push(node_idx);
            }
        }

        // Filter out tiny clusters -- merge into nearest large cluster.
        // Scale threshold with graph size: larger graphs need bigger clusters
        // to be considered meaningful subsystems. Minimum 3 for small graphs,
        // up to 10 for large graphs. Too high a threshold (>15) absorbs real
        // subsystems like "rerank" or "process" that are architecturally
        // distinct but small.
        let min_cluster_size = if prod_count < 50 {
            3
        } else {
            (prod_count / 200).clamp(5, 10)
        };
        let large_comms: HashSet<usize> = comm_members
            .iter()
            .filter(|(_, members)| members.len() >= min_cluster_size)
            .map(|(&comm, _)| comm)
            .collect();

        if large_comms.is_empty() {
            return Vec::new();
        }

        // Reassign small-cluster production nodes to the large cluster they
        // have the most edges to.
        for node in 0..n {
            if is_test_node[node] || large_comms.contains(&community[node]) {
                continue;
            }
            let mut best_comm = *large_comms.iter().next().unwrap();
            let mut best_weight = 0.0_f64;
            let mut comm_w: HashMap<usize, f64> = HashMap::new();
            for &(nbr, w) in &adj[node] {
                let c = community[nbr];
                if large_comms.contains(&c) {
                    *comm_w.entry(c).or_default() += w;
                }
            }
            for (&c, &w) in &comm_w {
                if w > best_weight {
                    best_weight = w;
                    best_comm = c;
                }
            }
            community[node] = best_comm;
        }

        // Step 5: Assign test nodes to their nearest production subsystem.
        // Each test node gets the community that has the most coupling edges
        // to it (using the full graph, not the filtered adjacency list).
        for node in 0..n {
            if !is_test_node[node] {
                continue;
            }
            let mut best_comm = *large_comms.iter().next().unwrap();
            let mut best_weight = 0.0_f64;
            let mut comm_w: HashMap<usize, f64> = HashMap::new();
            // Check all edges in the original graph for this test node
            for edge_ref in self.graph.edges(NodeIndex::new(node)) {
                let nbr = edge_ref.target().index();
                if !is_test_node[nbr] {
                    let c = community[nbr];
                    if large_comms.contains(&c) {
                        let w = edge_weight(&edge_ref.weight().edge_type);
                        *comm_w.entry(c).or_default() += w;
                    }
                }
            }
            // Also check incoming edges
            for edge_ref in self
                .graph
                .edges_directed(NodeIndex::new(node), Direction::Incoming)
            {
                let nbr = edge_ref.source().index();
                if !is_test_node[nbr] {
                    let c = community[nbr];
                    if large_comms.contains(&c) {
                        let w = edge_weight(&edge_ref.weight().edge_type);
                        *comm_w.entry(c).or_default() += w;
                    }
                }
            }
            for (&c, &w) in &comm_w {
                if w > best_weight {
                    best_weight = w;
                    best_comm = c;
                }
            }
            community[node] = best_comm;
        }

        // Rebuild community members after merging (includes test nodes now)
        let mut comm_members: HashMap<usize, Vec<usize>> = HashMap::new();
        for (node_idx, &comm) in community.iter().enumerate() {
            comm_members.entry(comm).or_default().push(node_idx);
        }

        // Build Subsystem structs
        let mut subsystems: Vec<Subsystem> = Vec::new();

        for (&_comm_id, members) in &comm_members {
            if members.len() < min_cluster_size {
                continue;
            }

            let member_set: HashSet<usize> = members.iter().copied().collect();

            // Compute cohesion: internal_edges / total_edges for members
            let mut internal_weight = 0.0_f64;
            let mut total_member_weight = 0.0_f64;
            for &node in members {
                for &(nbr, w) in &adj[node] {
                    total_member_weight += w;
                    if member_set.contains(&nbr) {
                        internal_weight += w;
                    }
                }
            }
            let cohesion = if total_member_weight > 0.0 {
                internal_weight / total_member_weight
            } else {
                0.0
            };

            // Collect member node IDs and module names
            let mut member_ids: Vec<String> = Vec::new();
            let mut module_names: Vec<String> = Vec::new();
            for &idx in members {
                if let Some(nr) = self.graph.node_weight(NodeIndex::new(idx)) {
                    member_ids.push(nr.id.clone());
                    if nr.node_type == "module" {
                        // Extract the node name from the stable ID: "root:file:name:kind"
                        if let Some(before_kind) = nr.id.rsplit_once(':') {
                            if let Some((_, name)) = before_kind.0.rsplit_once(':') {
                                module_names.push(name.to_string());
                            }
                        }
                    }
                }
            }

            // Cluster naming: prefer module names, fall back to file-path prefix
            let name = compute_cluster_name(&member_ids, node_file_map, &module_names);

            // Interface scoring: cross_cluster_degree * pagerank
            let mut interfaces: Vec<SubsystemInterface> = Vec::new();
            for &node in members {
                let cross_cluster_degree: usize = adj[node]
                    .iter()
                    .filter(|(nbr, _)| !member_set.contains(nbr))
                    .count();
                if cross_cluster_degree == 0 {
                    continue;
                }
                if let Some(nr) = self.graph.node_weight(NodeIndex::new(node)) {
                    let importance = pagerank_scores.get(&nr.id).copied().unwrap_or(0.0);
                    let interface_score = cross_cluster_degree as f64 * importance;
                    interfaces.push(SubsystemInterface {
                        node_id: nr.id.clone(),
                        node_type: nr.node_type.clone(),
                        interface_score,
                    });
                }
            }
            interfaces.sort_by(|a, b| {
                b.interface_score
                    .partial_cmp(&a.interface_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            interfaces.truncate(5); // top 5 interfaces per subsystem

            subsystems.push(Subsystem {
                name,
                symbol_count: members.len(),
                cohesion,
                interfaces,
                member_ids: member_ids.clone(),
                children: Vec::new(),
            });
        }

        // Sort by symbol count descending
        subsystems.sort_by(|a, b| b.symbol_count.cmp(&a.symbol_count));
        subsystems
    }
}

// ---------------------------------------------------------------------------
// Louvain algorithm helpers
// ---------------------------------------------------------------------------

/// Louvain Phase 1: iteratively move nodes to the community that maximizes
/// modularity gain. Returns a community assignment vector.
///
/// `gamma` is the resolution parameter: lower values produce fewer, larger
/// clusters. gamma=1.0 is standard Louvain.
fn louvain_phase1(
    adj: &[Vec<(usize, f64)>],
    total_weight: f64,
    gamma: f64,
    n: usize,
) -> Vec<usize> {
    let k: Vec<f64> = adj
        .iter()
        .map(|nbrs| nbrs.iter().map(|(_, w)| w).sum())
        .collect();

    let mut community: Vec<usize> = (0..n).collect();
    let m2 = 2.0 * total_weight;

    let mut sigma_tot: HashMap<usize, f64> = (0..n).map(|i| (i, k[i])).collect();

    let mut improved = true;
    let mut iterations = 0;
    while improved && iterations < 20 {
        improved = false;
        iterations += 1;

        for node in 0..n {
            if k[node] == 0.0 {
                continue;
            }

            let current_comm = community[node];

            let mut comm_weights: HashMap<usize, f64> = HashMap::new();
            for &(nbr, w) in &adj[node] {
                *comm_weights.entry(community[nbr]).or_default() += w;
            }

            let w_to_current = comm_weights.get(&current_comm).copied().unwrap_or(0.0);
            let sigma_current = sigma_tot.get(&current_comm).copied().unwrap_or(0.0);
            let ki = k[node];

            // Modularity gain from removing node from its current community.
            // The gamma factor scales the expected-edges penalty term.
            let remove_delta =
                -w_to_current / m2 + gamma * ki * (sigma_current - ki) / (m2 * m2);

            let mut best_comm = current_comm;
            let mut best_gain = 0.0;

            for (&candidate_comm, &w_to_candidate) in &comm_weights {
                if candidate_comm == current_comm {
                    continue;
                }
                let sigma_candidate =
                    sigma_tot.get(&candidate_comm).copied().unwrap_or(0.0);
                let insert_delta =
                    w_to_candidate / m2 - gamma * ki * sigma_candidate / (m2 * m2);
                let gain = remove_delta + insert_delta;
                if gain > best_gain {
                    best_gain = gain;
                    best_comm = candidate_comm;
                }
            }

            if best_comm != current_comm {
                *sigma_tot.entry(current_comm).or_default() -= ki;
                *sigma_tot.entry(best_comm).or_default() += ki;
                community[node] = best_comm;
                improved = true;
            }
        }
    }

    community
}

/// Louvain Phase 2: hierarchical contraction of communities.
///
/// After Phase 1 produces fine-grained communities, Phase 2 merges pairs
/// of communities that share strong inter-community coupling relative to
/// their sizes. This is a greedy agglomerative merge: repeatedly find the
/// pair with the highest merge score and combine them, until no pair
/// exceeds the merge threshold.
///
/// The merge score between communities A and B is:
///   cross_weight(A,B) / min(size_A, size_B)
///
/// This normalizes by the smaller community's size, so small clusters with
/// many edges to a larger cluster merge in (e.g., 6 extract/* fragments
/// merge into one "extract" super-cluster).
///
/// `gamma` controls the merge threshold: lower gamma = more merging.
fn louvain_phase2(
    adj: &[Vec<(usize, f64)>],
    mut community: Vec<usize>,
    gamma: f64,
    n: usize,
) -> Vec<usize> {
    // Count initial communities from production nodes only (nodes with
    // coupling edges). Isolated nodes (test nodes, etc.) don't participate
    // in Phase 2 and shouldn't inflate the community count.
    let initial_comm_count = (0..n)
        .filter(|&i| !adj[i].is_empty())
        .map(|i| community[i])
        .collect::<HashSet<_>>()
        .len();

    // Merge threshold: scale with gamma and the square root of the ratio
    // between target and initial community count. This balances between
    // over-merging (one giant cluster) and under-merging (91 fragments).
    //
    // For 2 communities: threshold = gamma * min(1.0, sqrt(12/2)) = gamma
    // For 12 communities: threshold = gamma * sqrt(1.0) = gamma (~0.8)
    // For 90 communities: threshold = gamma * sqrt(12/90) = 0.29
    let target_k = 12.0_f64;
    let ratio = (target_k / initial_comm_count.max(1) as f64).min(1.0);
    let merge_threshold = gamma * ratio.sqrt();

    // Size cap: don't merge if the result would exceed 30% of production
    // nodes. This prevents the "snowball" effect where one cluster absorbs
    // everything through incremental merges.
    let prod_count = (0..n).filter(|&i| !adj[i].is_empty()).count();
    let max_community_size = (prod_count as f64 * 0.30) as usize;

    let max_rounds = 200;

    for _ in 0..max_rounds {
        // Collect unique community IDs and their sizes
        let mut comm_sizes: HashMap<usize, usize> = HashMap::new();
        for node in 0..n {
            *comm_sizes.entry(community[node]).or_default() += 1;
        }

        let comm_ids: Vec<usize> = comm_sizes.keys().copied().collect();
        if comm_ids.len() <= 1 {
            break;
        }

        // Compute cross-community edge weights between all pairs
        let mut cross_weights: HashMap<(usize, usize), f64> = HashMap::new();
        for node in 0..n {
            let ca = community[node];
            for &(nbr, w) in &adj[node] {
                let cb = community[nbr];
                if ca < cb {
                    *cross_weights.entry((ca, cb)).or_default() += w;
                }
            }
        }

        // Find the pair with the highest merge score.
        let mut best_pair: Option<(usize, usize)> = None;
        let mut best_score = 0.0_f64;

        for (&(ca, cb), &w) in &cross_weights {
            let size_a = comm_sizes[&ca];
            let size_b = comm_sizes[&cb];
            if size_a + size_b > max_community_size {
                continue; // would create a giant cluster
            }
            let min_size = size_a.min(size_b) as f64;
            let score = w / min_size;
            if score > best_score {
                best_score = score;
                best_pair = Some((ca, cb));
            }
        }

        if best_score < merge_threshold {
            break; // no pair worth merging
        }

        // Merge: reassign all nodes from the smaller community to the larger
        if let Some((ca, cb)) = best_pair {
            let (from, to) = if comm_sizes[&ca] < comm_sizes[&cb] {
                (ca, cb)
            } else {
                (cb, ca)
            };
            for node in 0..n {
                if community[node] == from {
                    community[node] = to;
                }
            }
        }
    }

    community
}

// ---------------------------------------------------------------------------
// Subsystem detection types
// ---------------------------------------------------------------------------

/// A detected subsystem (community) in the code graph.
#[derive(Debug, Clone)]
pub struct Subsystem {
    /// Name derived from module nodes (preferred) or file-path prefix (fallback).
    pub name: String,
    /// Number of symbols in this subsystem.
    pub symbol_count: usize,
    /// Ratio of internal to total edge weight (0.0 to 1.0).
    pub cohesion: f64,
    /// Top interface functions, scored by cross_cluster_degree * pagerank.
    pub interfaces: Vec<SubsystemInterface>,
    /// Stable IDs of all member nodes in this subsystem.
    pub member_ids: Vec<String>,
    /// Child sub-modules when this is a hierarchical parent subsystem.
    pub children: Vec<Subsystem>,
}

/// An interface function at a subsystem boundary.
#[derive(Debug, Clone)]
pub struct SubsystemInterface {
    /// Node ID of the interface function/struct.
    pub node_id: String,
    /// Node type (e.g., "function", "struct").
    pub node_type: String,
    /// Combined interface score: cross_cluster_degree * pagerank.
    pub interface_score: f64,
}

/// Group flat subsystems by shared first path component into a hierarchy.
///
/// When multiple subsystems share the same prefix before `/` (e.g., `extract/Node`,
/// `extract/enrich`), they become children of a parent subsystem named after the
/// shared prefix. Single subsystems (no `/` or unique prefix) remain as-is.
///
/// Parent stats are aggregated: symbol_count = sum of children, cohesion = weighted
/// average by symbol count, interfaces = merged and re-sorted top 5, member_ids =
/// union of all children.
pub fn group_subsystems_by_prefix(subsystems: Vec<Subsystem>) -> Vec<Subsystem> {
    // Partition: subsystems with a `/` in their name vs. those without.
    // Group by first path component for those with `/`.
    let mut prefix_groups: std::collections::BTreeMap<String, Vec<Subsystem>> =
        std::collections::BTreeMap::new();
    let mut standalone: Vec<Subsystem> = Vec::new();

    for s in subsystems {
        if let Some(slash_pos) = s.name.find('/') {
            let prefix = s.name[..slash_pos].to_string();
            prefix_groups.entry(prefix).or_default().push(s);
        } else {
            standalone.push(s);
        }
    }

    let mut result: Vec<Subsystem> = standalone;

    for (prefix, children) in prefix_groups {
        if children.len() == 1 {
            // Single child -- no grouping needed, keep as leaf
            result.push(children.into_iter().next().unwrap());
        } else {
            // Aggregate stats from children
            let total_symbols: usize = children.iter().map(|c| c.symbol_count).sum();
            let weighted_cohesion: f64 = if total_symbols > 0 {
                children
                    .iter()
                    .map(|c| c.cohesion * c.symbol_count as f64)
                    .sum::<f64>()
                    / total_symbols as f64
            } else {
                0.0
            };
            let all_member_ids: Vec<String> = children
                .iter()
                .flat_map(|c| c.member_ids.iter().cloned())
                .collect();
            let mut all_interfaces: Vec<SubsystemInterface> = children
                .iter()
                .flat_map(|c| c.interfaces.iter().cloned())
                .collect();
            all_interfaces.sort_by(|a, b| {
                b.interface_score
                    .partial_cmp(&a.interface_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            all_interfaces.truncate(5);

            result.push(Subsystem {
                name: prefix,
                symbol_count: total_symbols,
                cohesion: weighted_cohesion,
                interfaces: all_interfaces,
                member_ids: all_member_ids,
                children,
            });
        }
    }

    // Sort by symbol count descending (matching detect_communities output order)
    result.sort_by(|a, b| b.symbol_count.cmp(&a.symbol_count));
    result
}

/// Derive a child sub-module name from the file paths of its member nodes.
///
/// Given a parent prefix (e.g., "server") and the member IDs of a child cluster,
/// returns the most common second-level directory component or file stem that
/// distinguishes this child from siblings.  For example, members mostly in
/// `src/server/graph.rs` -> "graph"; members in `src/extract/lsp.rs` -> "lsp".
///
/// Falls back to "sub" if no distinguishing component can be determined.
pub fn child_name_from_files(
    member_ids: &[String],
    node_file_map: &HashMap<String, String>,
    _parent_prefix: &str,
) -> String {
    let mut component_counts: BTreeMap<String, usize> = BTreeMap::new();

    for id in member_ids {
        if let Some(file_path) = node_file_map.get(id.as_str()) {
            let parts: Vec<&str> = file_path.split('/').collect();
            // Strip "src/" prefix if present
            let parts = if parts.first() == Some(&"src") {
                &parts[1..]
            } else {
                &parts[..]
            };

            if parts.len() >= 2 {
                // "server/graph.rs" -> "graph", "server/graph/mod.rs" -> "graph"
                let component = if parts.len() == 2 {
                    parts[1]
                        .rsplit_once('.')
                        .map(|(stem, _)| stem)
                        .unwrap_or(parts[1])
                } else {
                    parts[1]
                };
                if component != "mod" {
                    *component_counts.entry(component.to_string()).or_default() += 1;
                }
            } else if parts.len() == 1 {
                // Flat file in src/ (e.g., "embed.rs") -> use file stem
                let stem = parts[0]
                    .rsplit_once('.')
                    .map(|(s, _)| s)
                    .unwrap_or(parts[0]);
                if !stem.is_empty() {
                    *component_counts.entry(stem.to_string()).or_default() += 1;
                }
            }
        }
    }

    if let Some((name, _)) = component_counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
    {
        return name;
    }

    "sub".to_string()
}

/// Compute a cluster name from member nodes.
///
/// Priority:
/// 1. If the cluster contains `Module` nodes, use the most common module name.
/// 2. Otherwise, use the deepest common directory prefix of member file paths.
/// 3. Never use function, struct, enum, or const names.
fn compute_cluster_name(
    member_ids: &[String],
    node_file_map: &HashMap<String, String>,
    module_names: &[String],
) -> String {
    // Priority 1: Use module names if any exist in this cluster
    if !module_names.is_empty() {
        // Pick the most common module name (handles clusters with multiple modules).
        // Use BTreeMap for deterministic tie-breaking (lexicographic order).
        let mut name_counts: BTreeMap<&str, usize> = BTreeMap::new();
        for name in module_names {
            *name_counts.entry(name.as_str()).or_default() += 1;
        }
        if let Some((name, _)) = name_counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
        {
            return name.to_string();
        }
    }

    // Priority 2: Deepest common directory prefix of member file paths.
    // Compute the longest common directory path across all members, stripping
    // "src/" prefix and file basenames. E.g., ["src/server/handlers/a.rs",
    // "src/server/handlers/b.rs"] -> "server/handlers".
    let mut dirs: Vec<Vec<&str>> = Vec::new();
    for id in member_ids {
        if let Some(file_path) = node_file_map.get(id.as_str()) {
            let parts: Vec<&str> = file_path.split('/').collect();
            if parts.is_empty() {
                continue;
            }
            // Strip filename (last component); keep directory components
            let dir_parts = &parts[..parts.len().saturating_sub(1)];
            // Strip leading "src/" if present
            let dir_parts = if dir_parts.first() == Some(&"src") {
                &dir_parts[1..]
            } else {
                dir_parts
            };
            if dir_parts.is_empty() {
                // File directly in src/ (e.g., src/server.rs) -- use stem as name
                if let Some(stem) = parts.last().and_then(|f| f.strip_suffix(".rs")) {
                    dirs.push(vec![stem]);
                }
            } else {
                dirs.push(dir_parts.to_vec());
            }
        }
    }

    if let Some(first) = dirs.first() {
        let mut common = first.clone();
        for d in dirs.iter().skip(1) {
            let mut i = 0;
            while i < common.len() && i < d.len() && common[i] == d[i] {
                i += 1;
            }
            common.truncate(i);
            if common.is_empty() {
                break;
            }
        }
        if !common.is_empty() {
            return common.join("/");
        }
    }

    // Priority 3: When no common prefix exists (mixed directories), use the
    // most frequent first directory component.
    if !dirs.is_empty() {
        let mut first_counts: BTreeMap<&str, usize> = BTreeMap::new();
        for d in &dirs {
            if let Some(&first) = d.first() {
                *first_counts.entry(first).or_default() += 1;
            }
        }
        if let Some((name, _)) = first_counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
        {
            return name.to_string();
        }
    }

    "unknown".to_string()
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
    /// graph traversal handler. Returns (all_ids_deduped, entry_nodes_stripped).
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

    // ==================== neighbors_grouped tests ====================

    #[test]
    fn test_neighbors_grouped_basic() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("a", "fn", "c", "struct", EdgeKind::DependsOn);
        index.add_edge("a", "fn", "d", "fn", EdgeKind::Calls);

        let groups = index.neighbors_grouped("a", None, Direction::Outgoing);
        assert_eq!(groups.len(), 2, "should have 2 edge types");
        assert_eq!(groups[&EdgeKind::Calls].len(), 2, "should have 2 Calls neighbors");
        assert_eq!(groups[&EdgeKind::DependsOn].len(), 1, "should have 1 DependsOn neighbor");
        assert!(groups[&EdgeKind::Calls].contains(&"b".to_string()));
        assert!(groups[&EdgeKind::Calls].contains(&"d".to_string()));
        assert!(groups[&EdgeKind::DependsOn].contains(&"c".to_string()));
    }

    #[test]
    fn test_neighbors_grouped_empty_for_nonexistent_node() {
        let index = GraphIndex::new();
        let groups = index.neighbors_grouped("nonexistent", None, Direction::Outgoing);
        assert!(groups.is_empty());
    }

    #[test]
    fn test_neighbors_grouped_with_edge_filter() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("a", "fn", "c", "struct", EdgeKind::DependsOn);
        index.add_edge("a", "fn", "d", "fn", EdgeKind::Implements);

        let groups = index.neighbors_grouped("a", Some(&[EdgeKind::Calls, EdgeKind::Implements]), Direction::Outgoing);
        assert_eq!(groups.len(), 2, "should only have filtered edge types");
        assert!(!groups.contains_key(&EdgeKind::DependsOn));
        assert_eq!(groups[&EdgeKind::Calls].len(), 1);
        assert_eq!(groups[&EdgeKind::Implements].len(), 1);
    }

    #[test]
    fn test_neighbors_grouped_incoming() {
        let mut index = GraphIndex::new();
        index.add_edge("x", "fn", "a", "fn", EdgeKind::Calls);
        index.add_edge("y", "fn", "a", "fn", EdgeKind::DependsOn);
        index.add_edge("z", "fn", "a", "fn", EdgeKind::Calls);

        let groups = index.neighbors_grouped("a", None, Direction::Incoming);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[&EdgeKind::Calls].len(), 2);
        assert_eq!(groups[&EdgeKind::DependsOn].len(), 1);
    }

    #[test]
    fn test_neighbors_grouped_no_empty_groups() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);

        let groups = index.neighbors_grouped("a", None, Direction::Outgoing);
        // Should only have the one edge type that has results
        assert_eq!(groups.len(), 1);
        assert!(groups.contains_key(&EdgeKind::Calls));
        // Other edge types should not appear
        assert!(!groups.contains_key(&EdgeKind::DependsOn));
        assert!(!groups.contains_key(&EdgeKind::Defines));
    }

    // ==================== Community detection tests ====================

    /// Helper to build a file map for community detection tests.
    fn make_file_map(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(id, file)| (id.to_string(), file.to_string()))
            .collect()
    }

    #[test]
    fn test_detect_communities_empty_graph() {
        let index = GraphIndex::new();
        let subsystems = index.detect_communities(&HashMap::new(), &HashMap::new());
        assert!(subsystems.is_empty());
    }

    #[test]
    fn test_detect_communities_no_coupling_edges() {
        // Only Defines edges -- should produce no subsystems since they are excluded
        let mut index = GraphIndex::new();
        for i in 0..5 {
            index.add_edge(
                &format!("struct_{}", i),
                "struct",
                &format!("field_{}", i),
                "field",
                EdgeKind::Defines,
            );
        }
        let subsystems = index.detect_communities(&HashMap::new(), &HashMap::new());
        assert!(
            subsystems.is_empty(),
            "Defines-only graph should produce no subsystems"
        );
    }

    #[test]
    fn test_detect_communities_two_clusters() {
        // Two dense clusters of 6 nodes each with a single weak inter-cluster
        // link. Needs 6 nodes per cluster so the modularity signal is strong
        // enough to resist merging under gamma=0.8 + Phase 2 contraction.
        let mut index = GraphIndex::new();

        // Cluster A: fully connected 6-node clique
        let a_nodes: Vec<String> = (1..=6).map(|i| format!("a{}", i)).collect();
        for i in 0..a_nodes.len() {
            for j in 0..a_nodes.len() {
                if i != j {
                    index.add_edge(&a_nodes[i], "fn", &a_nodes[j], "fn", EdgeKind::Calls);
                }
            }
        }

        // Cluster B: fully connected 6-node clique
        let b_nodes: Vec<String> = (1..=6).map(|i| format!("b{}", i)).collect();
        for i in 0..b_nodes.len() {
            for j in 0..b_nodes.len() {
                if i != j {
                    index.add_edge(&b_nodes[i], "fn", &b_nodes[j], "fn", EdgeKind::Calls);
                }
            }
        }

        // Single weak inter-cluster link
        index.add_edge("a6", "fn", "b1", "fn", EdgeKind::Calls);

        let pagerank = index.compute_pagerank(0.85, 20);
        let mut file_map_entries: Vec<(&str, &str)> = Vec::new();
        for n in &a_nodes {
            file_map_entries.push((n.as_str(), "src/alpha/mod.rs"));
        }
        for n in &b_nodes {
            file_map_entries.push((n.as_str(), "src/beta/mod.rs"));
        }
        let file_map: HashMap<String, String> = file_map_entries
            .iter()
            .map(|(id, file)| (id.to_string(), file.to_string()))
            .collect();

        let subsystems = index.detect_communities(&pagerank, &file_map);
        assert!(
            subsystems.len() >= 2,
            "should detect at least 2 clusters, got {}",
            subsystems.len()
        );

        // Verify cluster names come from file paths
        let names: Vec<&str> = subsystems.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"alpha") || names.contains(&"beta"),
            "cluster names should reflect file paths, got {:?}",
            names
        );
    }

    #[test]
    fn test_detect_communities_has_interfaces() {
        // Two dense 6-node clusters with a cross-cluster link; the boundary
        // nodes should be detected as interface functions.
        let mut index = GraphIndex::new();

        // Cluster A: fully connected 6-node clique
        let a_nodes: Vec<String> = (1..=6).map(|i| format!("a{}", i)).collect();
        for i in 0..a_nodes.len() {
            for j in 0..a_nodes.len() {
                if i != j {
                    index.add_edge(&a_nodes[i], "fn", &a_nodes[j], "fn", EdgeKind::Calls);
                }
            }
        }

        // Cluster B: fully connected 6-node clique
        let b_nodes: Vec<String> = (1..=6).map(|i| format!("b{}", i)).collect();
        for i in 0..b_nodes.len() {
            for j in 0..b_nodes.len() {
                if i != j {
                    index.add_edge(&b_nodes[i], "fn", &b_nodes[j], "fn", EdgeKind::Calls);
                }
            }
        }

        // Cross-cluster link
        index.add_edge("a6", "fn", "b1", "fn", EdgeKind::Calls);

        let pagerank = index.compute_pagerank(0.85, 20);
        let mut file_map_entries: Vec<(&str, &str)> = Vec::new();
        for n in &a_nodes {
            file_map_entries.push((n.as_str(), "src/alpha/mod.rs"));
        }
        for n in &b_nodes {
            file_map_entries.push((n.as_str(), "src/beta/mod.rs"));
        }
        let file_map: HashMap<String, String> = file_map_entries
            .iter()
            .map(|(id, file)| (id.to_string(), file.to_string()))
            .collect();

        let subsystems = index.detect_communities(&pagerank, &file_map);
        // At least one subsystem should have interface functions
        let total_interfaces: usize = subsystems.iter().map(|s| s.interfaces.len()).sum();
        assert!(
            total_interfaces > 0,
            "cross-cluster edges should produce interface functions"
        );

        // The interface nodes should be the ones on the boundary (a6 and/or b1)
        let interface_ids: Vec<&str> = subsystems
            .iter()
            .flat_map(|s| s.interfaces.iter().map(|i| i.node_id.as_str()))
            .collect();
        assert!(
            interface_ids.contains(&"a6") || interface_ids.contains(&"b1"),
            "boundary nodes should be interfaces, got {:?}",
            interface_ids
        );
    }

    #[test]
    fn test_detect_communities_cohesion_range() {
        // Build a well-connected cluster
        let mut index = GraphIndex::new();
        for i in 0..5 {
            for j in 0..5 {
                if i != j {
                    index.add_edge(
                        &format!("n{}", i),
                        "fn",
                        &format!("n{}", j),
                        "fn",
                        EdgeKind::Calls,
                    );
                }
            }
        }

        let pagerank = index.compute_pagerank(0.85, 20);
        let file_map: HashMap<String, String> = (0..5)
            .map(|i| (format!("n{}", i), "src/cluster/mod.rs".to_string()))
            .collect();

        let subsystems = index.detect_communities(&pagerank, &file_map);
        // Should detect one cluster with high cohesion
        if !subsystems.is_empty() {
            for s in &subsystems {
                assert!(
                    s.cohesion >= 0.0 && s.cohesion <= 1.0,
                    "cohesion should be in [0,1], got {}",
                    s.cohesion
                );
            }
        }
    }

    #[test]
    fn test_detect_communities_excludes_defines_edges() {
        // Defines edges should NOT contribute to community detection.
        // Two separate groups connected only by Defines should not form one community.
        let mut index = GraphIndex::new();

        // Group A: fully connected 6-node clique via Calls
        let a_nodes: Vec<String> = (1..=6).map(|i| format!("a{}", i)).collect();
        for i in 0..a_nodes.len() {
            for j in 0..a_nodes.len() {
                if i != j {
                    index.add_edge(&a_nodes[i], "fn", &a_nodes[j], "fn", EdgeKind::Calls);
                }
            }
        }

        // Group B: fully connected 6-node clique via Calls
        let b_nodes: Vec<String> = (1..=6).map(|i| format!("b{}", i)).collect();
        for i in 0..b_nodes.len() {
            for j in 0..b_nodes.len() {
                if i != j {
                    index.add_edge(&b_nodes[i], "fn", &b_nodes[j], "fn", EdgeKind::Calls);
                }
            }
        }

        // Connect A and B ONLY via Defines edges (should be ignored for clustering)
        index.add_edge("a1", "struct", "b1", "field", EdgeKind::Defines);
        index.add_edge("a2", "struct", "b2", "field", EdgeKind::Defines);

        let pagerank = index.compute_pagerank(0.85, 20);
        let mut file_map_entries: Vec<(&str, &str)> = Vec::new();
        for n in &a_nodes {
            file_map_entries.push((n.as_str(), "src/alpha/mod.rs"));
        }
        for n in &b_nodes {
            file_map_entries.push((n.as_str(), "src/beta/mod.rs"));
        }
        let file_map: HashMap<String, String> = file_map_entries
            .iter()
            .map(|(id, file)| (id.to_string(), file.to_string()))
            .collect();

        let subsystems = index.detect_communities(&pagerank, &file_map);
        // Should detect 2 separate clusters (A and B), not merge them via Defines
        assert!(
            subsystems.len() >= 2,
            "Groups connected only by Defines edges should remain separate clusters, got {} subsystem(s)",
            subsystems.len()
        );
    }

    #[test]
    fn test_detect_communities_single_dense_cluster() {
        // All nodes tightly connected -- should produce exactly one subsystem
        let mut index = GraphIndex::new();
        let node_count = 6;
        for i in 0..node_count {
            for j in 0..node_count {
                if i != j {
                    index.add_edge(
                        &format!("n{}", i),
                        "fn",
                        &format!("n{}", j),
                        "fn",
                        EdgeKind::Calls,
                    );
                }
            }
        }

        let pagerank = index.compute_pagerank(0.85, 20);
        let file_map: HashMap<String, String> = (0..node_count)
            .map(|i| (format!("n{}", i), "src/core/mod.rs".to_string()))
            .collect();

        let subsystems = index.detect_communities(&pagerank, &file_map);
        assert_eq!(
            subsystems.len(),
            1,
            "fully connected graph should be one subsystem, got {}",
            subsystems.len()
        );
        assert_eq!(subsystems[0].symbol_count, node_count);
        // High cohesion expected (no external edges)
        assert!(
            subsystems[0].cohesion > 0.9,
            "single cluster should have high cohesion, got {}",
            subsystems[0].cohesion
        );
        // No interfaces since there's only one cluster
        assert!(
            subsystems[0].interfaces.is_empty(),
            "single cluster should have no interfaces"
        );
    }

    #[test]
    fn test_compute_cluster_name_file_path_fallback() {
        let file_map = make_file_map(&[
            ("a", "src/graph/index.rs"),
            ("b", "src/graph/mod.rs"),
            ("c", "src/graph/store.rs"),
            ("d", "src/server.rs"),
        ]);
        let no_modules: Vec<String> = vec![];

        // No module nodes -> falls back to file-path prefix
        // Majority in src/graph/ -> name should be "graph"
        let name = compute_cluster_name(
            &["a".to_string(), "b".to_string(), "c".to_string()],
            &file_map,
            &no_modules,
        );
        assert_eq!(name, "graph");

        // Mixed: 2 graph + 1 server -> still "graph"
        let name = compute_cluster_name(
            &["a".to_string(), "b".to_string(), "d".to_string()],
            &file_map,
            &no_modules,
        );
        assert_eq!(name, "graph");
    }

    #[test]
    fn test_compute_cluster_name_prefers_module_nodes() {
        let file_map = make_file_map(&[
            ("a", "src/graph/index.rs"),
            ("b", "src/graph/mod.rs"),
            ("c", "src/server.rs"),
        ]);

        // Module node present -> use its name instead of file-path prefix
        let module_names = vec!["graph".to_string()];
        let name = compute_cluster_name(
            &["a".to_string(), "b".to_string(), "c".to_string()],
            &file_map,
            &module_names,
        );
        assert_eq!(name, "graph");

        // Module name wins even when file-path majority differs
        let file_map2 = make_file_map(&[
            ("a", "src/server/handler.rs"),
            ("b", "src/server/routes.rs"),
            ("c", "src/server/mod.rs"),
        ]);
        let module_names = vec!["my_server".to_string()];
        let name = compute_cluster_name(
            &["a".to_string(), "b".to_string(), "c".to_string()],
            &file_map2,
            &module_names,
        );
        assert_eq!(name, "my_server");
    }

    #[test]
    fn test_compute_cluster_name_most_common_module() {
        let file_map = HashMap::new();
        // Multiple module nodes -> pick the most common
        let module_names = vec![
            "extract".to_string(),
            "extract".to_string(),
            "scanner".to_string(),
        ];
        let name = compute_cluster_name(&[], &file_map, &module_names);
        assert_eq!(name, "extract");
    }

    #[test]
    fn test_compute_cluster_name_deepest_common_prefix() {
        let no_modules: Vec<String> = vec![];

        // All files in same nested directory -> uses deepest common prefix
        let file_map = make_file_map(&[
            ("a", "src/server/handlers/auth.rs"),
            ("b", "src/server/handlers/user.rs"),
        ]);
        let name = compute_cluster_name(
            &["a".to_string(), "b".to_string()],
            &file_map,
            &no_modules,
        );
        assert_eq!(name, "server/handlers");

        // Mixed nested paths -> common prefix is "server"
        let file_map = make_file_map(&[
            ("a", "src/server/handlers/auth.rs"),
            ("b", "src/server/routes.rs"),
        ]);
        let name = compute_cluster_name(
            &["a".to_string(), "b".to_string()],
            &file_map,
            &no_modules,
        );
        assert_eq!(name, "server");
    }

    #[test]
    fn test_detect_communities_excludes_depends_on() {
        // DependsOn edges should NOT contribute to community detection.
        // Two separate groups connected only by DependsOn should not form one community.
        let mut index = GraphIndex::new();

        // Group A: fully connected 6-node clique via Calls
        let a_nodes: Vec<String> = (1..=6).map(|i| format!("a{}", i)).collect();
        for i in 0..a_nodes.len() {
            for j in 0..a_nodes.len() {
                if i != j {
                    index.add_edge(&a_nodes[i], "fn", &a_nodes[j], "fn", EdgeKind::Calls);
                }
            }
        }

        // Group B: fully connected 6-node clique via Calls
        let b_nodes: Vec<String> = (1..=6).map(|i| format!("b{}", i)).collect();
        for i in 0..b_nodes.len() {
            for j in 0..b_nodes.len() {
                if i != j {
                    index.add_edge(&b_nodes[i], "fn", &b_nodes[j], "fn", EdgeKind::Calls);
                }
            }
        }

        // Connect A and B ONLY via DependsOn edges (should be ignored for clustering)
        index.add_edge("a1", "fn", "b1", "module", EdgeKind::DependsOn);
        index.add_edge("a2", "fn", "b2", "module", EdgeKind::DependsOn);

        let pagerank = index.compute_pagerank(0.85, 20);
        let mut file_map_entries: Vec<(&str, &str)> = Vec::new();
        for n in &a_nodes {
            file_map_entries.push((n.as_str(), "src/alpha/mod.rs"));
        }
        for n in &b_nodes {
            file_map_entries.push((n.as_str(), "src/beta/mod.rs"));
        }
        let file_map: HashMap<String, String> = file_map_entries
            .iter()
            .map(|(id, file)| (id.to_string(), file.to_string()))
            .collect();

        let subsystems = index.detect_communities(&pagerank, &file_map);
        // Should detect 2 separate clusters (A and B), not merge them via DependsOn
        assert!(
            subsystems.len() >= 2,
            "Groups connected only by DependsOn edges should remain separate clusters, got {} subsystem(s)",
            subsystems.len()
        );
    }

    #[test]
    fn test_detect_communities_uses_module_names() {
        // When a cluster contains a Module node, the subsystem name should come
        // from the module node's name, not from file paths or function names.
        // Use realistic stable IDs (root:file:name:kind) since the extraction
        // relies on splitting the ID to find the module name.
        let mut index = GraphIndex::new();

        let mod_id = "test:src/graph/mod.rs:graph:module";
        let fn1_id = "test:src/graph/index.rs:build_graph:function";
        let fn2_id = "test:src/graph/index.rs:add_edge:function";

        // Cluster with a module node and several functions
        index.add_edge(mod_id, "module", fn1_id, "fn", EdgeKind::Defines);
        index.add_edge(fn1_id, "fn", fn2_id, "fn", EdgeKind::Calls);
        index.add_edge(fn2_id, "fn", fn1_id, "fn", EdgeKind::Calls);
        index.add_edge(fn1_id, "fn", mod_id, "module", EdgeKind::Calls);
        index.add_edge(fn2_id, "fn", mod_id, "module", EdgeKind::Calls);
        index.add_edge(mod_id, "module", fn2_id, "fn", EdgeKind::Calls);

        let pagerank = index.compute_pagerank(0.85, 20);
        let file_map: HashMap<String, String> = [
            (mod_id, "src/graph/mod.rs"),
            (fn1_id, "src/graph/index.rs"),
            (fn2_id, "src/graph/index.rs"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let subsystems = index.detect_communities(&pagerank, &file_map);

        // With 3 nodes and 5 coupling edges, Louvain should form a cluster
        assert!(
            !subsystems.is_empty(),
            "expected at least one subsystem from a densely-connected 3-node graph"
        );

        let names: Vec<&str> = subsystems.iter().map(|s| s.name.as_str()).collect();

        // Positive assertion: module-derived name "graph" should be used
        assert!(
            names.contains(&"graph"),
            "subsystem should be named after the module node 'graph', got {:?}",
            names
        );
        // Negative assertion: should NOT contain function names
        assert!(
            !names.contains(&"build_graph") && !names.contains(&"add_edge"),
            "subsystem names should not be function names, got {:?}",
            names
        );
    }

    #[test]
    fn test_detect_communities_density_guard() {
        // A large sparse graph with very few coupling edges should produce no subsystems.
        // This tests the density guard that prevents garbage results when LSP data
        // is unavailable.
        let mut index = GraphIndex::new();

        // Add 200 nodes connected only by Defines edges (no coupling signal)
        for i in 0..100 {
            index.add_edge(
                &format!("struct_{}", i),
                "struct",
                &format!("field_{}", i),
                "field",
                EdgeKind::Defines,
            );
        }
        // Add just 2 coupling edges (far below 5% of 200 nodes)
        index.add_edge("struct_0", "struct", "struct_1", "struct", EdgeKind::Calls);
        index.add_edge("struct_1", "struct", "struct_0", "struct", EdgeKind::Calls);

        let subsystems = index.detect_communities(&HashMap::new(), &HashMap::new());
        assert!(
            subsystems.is_empty(),
            "Sparse graph with minimal coupling edges should produce no subsystems, got {}",
            subsystems.len()
        );
    }

    #[test]
    fn test_detect_communities_excludes_test_nodes() {
        // Test nodes should NOT form their own subsystems. They should be
        // filtered from Louvain and assigned to their nearest production
        // subsystem post-hoc.
        let mut index = GraphIndex::new();

        // Production cluster: fully connected 6-node clique
        let prod_nodes: Vec<String> = (1..=6).map(|i| format!("p{}", i)).collect();
        for i in 0..prod_nodes.len() {
            for j in 0..prod_nodes.len() {
                if i != j {
                    index.add_edge(
                        &prod_nodes[i],
                        "fn",
                        &prod_nodes[j],
                        "fn",
                        EdgeKind::Calls,
                    );
                }
            }
        }

        // Test cluster: 6 test nodes tightly interconnected
        let test_nodes: Vec<String> = (1..=6).map(|i| format!("t{}", i)).collect();
        for i in 0..test_nodes.len() {
            for j in 0..test_nodes.len() {
                if i != j {
                    index.add_edge(
                        &test_nodes[i],
                        "fn",
                        &test_nodes[j],
                        "fn",
                        EdgeKind::Calls,
                    );
                }
            }
        }

        // Test nodes call production nodes (tests import production code)
        index.add_edge("t1", "fn", "p1", "fn", EdgeKind::Calls);
        index.add_edge("t2", "fn", "p2", "fn", EdgeKind::Calls);

        let pagerank = index.compute_pagerank(0.85, 20);

        // Map production nodes to src/core/, test nodes to src/core/tests/
        let mut file_map = HashMap::new();
        for n in &prod_nodes {
            file_map.insert(n.clone(), "src/core/mod.rs".to_string());
        }
        for n in &test_nodes {
            file_map.insert(n.clone(), "src/core/tests/mod.rs".to_string());
        }

        let subsystems = index.detect_communities(&pagerank, &file_map);

        // Should produce exactly 1 subsystem (production), not 2
        // Test nodes are folded into the production subsystem
        assert_eq!(
            subsystems.len(),
            1,
            "test nodes should not form their own subsystem, got {} subsystems: {:?}",
            subsystems.len(),
            subsystems.iter().map(|s| &s.name).collect::<Vec<_>>()
        );

        // The subsystem should contain both production and test nodes
        let total_symbols: usize = subsystems.iter().map(|s| s.symbol_count).sum();
        assert!(
            total_symbols >= prod_nodes.len(),
            "subsystem should contain at least all production nodes"
        );
    }

    #[test]
    fn test_detect_communities_phase2_contraction() {
        // Phase 2 contraction should merge sub-clusters that belong together.
        // Build 4 mini-clusters (4 nodes each) with many inter-pair links:
        //   A1-A2 are densely linked (should merge in Phase 2)
        //   B1-B2 are densely linked (should merge in Phase 2)
        // But A-pair and B-pair have only a single weak link.
        let mut index = GraphIndex::new();

        // Helper: build a 4-node fully connected clique
        fn add_clique(index: &mut GraphIndex, prefix: &str) -> Vec<String> {
            let nodes: Vec<String> = (1..=4).map(|i| format!("{}{}", prefix, i)).collect();
            for i in 0..nodes.len() {
                for j in 0..nodes.len() {
                    if i != j {
                        index.add_edge(&nodes[i], "fn", &nodes[j], "fn", EdgeKind::Calls);
                    }
                }
            }
            nodes
        }

        let a1 = add_clique(&mut index, "a1_");
        let a2 = add_clique(&mut index, "a2_");
        let b1 = add_clique(&mut index, "b1_");
        let b2 = add_clique(&mut index, "b2_");

        // Dense inter-links within A-pair: every a1 node connects to every a2 node
        for a1n in &a1 {
            for a2n in &a2 {
                index.add_edge(a1n, "fn", a2n, "fn", EdgeKind::Calls);
                index.add_edge(a2n, "fn", a1n, "fn", EdgeKind::Calls);
            }
        }

        // Dense inter-links within B-pair: every b1 node connects to every b2 node
        for b1n in &b1 {
            for b2n in &b2 {
                index.add_edge(b1n, "fn", b2n, "fn", EdgeKind::Calls);
                index.add_edge(b2n, "fn", b1n, "fn", EdgeKind::Calls);
            }
        }

        // Single weak link between A-pair and B-pair
        index.add_edge(&a1[3], "fn", &b1[3], "fn", EdgeKind::Calls);

        let pagerank = index.compute_pagerank(0.85, 20);
        let mut file_map = HashMap::new();
        for n in a1.iter().chain(a2.iter()) {
            file_map.insert(n.clone(), "src/alpha/mod.rs".to_string());
        }
        for n in b1.iter().chain(b2.iter()) {
            file_map.insert(n.clone(), "src/beta/mod.rs".to_string());
        }

        let subsystems = index.detect_communities(&pagerank, &file_map);
        // With dense inter-pair links, Phase 1 or Phase 2 should merge a1+a2
        // and b1+b2, yielding at most 3 subsystems (ideally 2).
        assert!(
            subsystems.len() <= 3,
            "Phase 2 contraction should merge sub-clusters, got {} subsystems",
            subsystems.len()
        );
        assert!(
            subsystems.len() >= 2,
            "should still keep distinct architectural groups separate, got {} subsystem(s)",
            subsystems.len()
        );
    }

    #[test]
    fn test_group_subsystems_by_prefix_groups_shared_prefix() {
        use super::{group_subsystems_by_prefix, Subsystem};
        let subsystems = vec![
            Subsystem {
                name: "extract/Node".into(),
                symbol_count: 100,
                cohesion: 0.8,
                interfaces: vec![],
                member_ids: vec!["a".into()],
                children: vec![],
            },
            Subsystem {
                name: "extract/enrich".into(),
                symbol_count: 50,
                cohesion: 0.9,
                interfaces: vec![],
                member_ids: vec!["b".into()],
                children: vec![],
            },
            Subsystem {
                name: "graph".into(),
                symbol_count: 200,
                cohesion: 0.85,
                interfaces: vec![],
                member_ids: vec!["c".into()],
                children: vec![],
            },
        ];
        let grouped = group_subsystems_by_prefix(subsystems);
        assert_eq!(grouped.len(), 2, "Should have 2 top-level: graph + extract");
        // Sorted by symbol_count desc: graph(200), extract(150)
        assert_eq!(grouped[0].name, "graph");
        assert!(grouped[0].children.is_empty());
        assert_eq!(grouped[1].name, "extract");
        assert_eq!(grouped[1].symbol_count, 150);
        assert_eq!(grouped[1].children.len(), 2);
        assert_eq!(grouped[1].member_ids.len(), 2); // "a" + "b"
    }

    #[test]
    fn test_group_subsystems_by_prefix_single_child_no_group() {
        use super::{group_subsystems_by_prefix, Subsystem};
        let subsystems = vec![Subsystem {
            name: "embed/model".into(),
            symbol_count: 30,
            cohesion: 0.9,
            interfaces: vec![],
            member_ids: vec!["x".into()],
            children: vec![],
        }];
        let grouped = group_subsystems_by_prefix(subsystems);
        assert_eq!(grouped.len(), 1);
        // Single child: kept as leaf, not wrapped in parent
        assert_eq!(grouped[0].name, "embed/model");
        assert!(grouped[0].children.is_empty());
    }

    #[test]
    fn test_child_name_from_files_uses_directory_component() {
        use super::child_name_from_files;
        let mut node_file_map = HashMap::new();
        node_file_map.insert("id1".to_string(), "src/server/graph.rs".to_string());
        node_file_map.insert("id2".to_string(), "src/server/graph.rs".to_string());
        node_file_map.insert("id3".to_string(), "src/server/graph.rs".to_string());
        node_file_map.insert("id4".to_string(), "src/server/tools.rs".to_string());

        let member_ids: Vec<String> = vec!["id1", "id2", "id3", "id4"]
            .into_iter()
            .map(String::from)
            .collect();
        let name = child_name_from_files(&member_ids, &node_file_map, "server");
        assert_eq!(name, "graph", "Should use most-common second-level dir component");
    }

    #[test]
    fn test_child_name_from_files_nested_directory() {
        use super::child_name_from_files;
        let mut node_file_map = HashMap::new();
        node_file_map.insert("id1".to_string(), "src/extract/lsp/mod.rs".to_string());
        node_file_map.insert("id2".to_string(), "src/extract/lsp/enricher.rs".to_string());

        let member_ids: Vec<String> = vec!["id1", "id2"]
            .into_iter()
            .map(String::from)
            .collect();
        let name = child_name_from_files(&member_ids, &node_file_map, "extract");
        assert_eq!(name, "lsp", "Should use second-level directory name");
    }

    #[test]
    fn test_child_name_from_files_flat_src_files() {
        use super::child_name_from_files;
        let mut node_file_map = HashMap::new();
        node_file_map.insert("id1".to_string(), "src/embed.rs".to_string());
        node_file_map.insert("id2".to_string(), "src/embed.rs".to_string());

        let member_ids: Vec<String> = vec!["id1", "id2"]
            .into_iter()
            .map(String::from)
            .collect();
        let name = child_name_from_files(&member_ids, &node_file_map, "server");
        assert_eq!(name, "embed", "Should use file stem for flat src/ files");
    }

    #[test]
    fn test_group_subsystems_weighted_cohesion() {
        use super::{group_subsystems_by_prefix, Subsystem};
        let subsystems = vec![
            Subsystem {
                name: "md/parse".into(),
                symbol_count: 60,
                cohesion: 0.8,
                interfaces: vec![],
                member_ids: vec![],
                children: vec![],
            },
            Subsystem {
                name: "md/search".into(),
                symbol_count: 40,
                cohesion: 0.9,
                interfaces: vec![],
                member_ids: vec![],
                children: vec![],
            },
        ];
        let grouped = group_subsystems_by_prefix(subsystems);
        assert_eq!(grouped.len(), 1);
        let parent = &grouped[0];
        // Weighted average: (0.8*60 + 0.9*40) / 100 = 84/100 = 0.84
        assert!((parent.cohesion - 0.84).abs() < 0.01);
    }

    // ── detect_cycles / cycle_for_node tests ────────────────────────────

    #[test]
    fn test_detect_cycles_no_cycles() {
        // a -> b -> c (DAG — no cycle)
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);

        let rings = index.detect_cycles(None);
        assert!(rings.is_empty(), "DAG should have no cycles");
    }

    #[test]
    fn test_detect_cycles_simple_ring() {
        // a -> b -> c -> a
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "a", "fn", EdgeKind::Calls);

        let rings = index.detect_cycles(None);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 3);
        // All three nodes are in the ring
        for node in &["a", "b", "c"] {
            assert!(rings[0].contains(&node.to_string()), "{} should be in ring", node);
        }
    }

    #[test]
    fn test_detect_cycles_two_independent_rings() {
        // Ring 1: a -> b -> a
        // Ring 2: x -> y -> z -> x
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "a", "fn", EdgeKind::Calls);
        index.add_edge("x", "fn", "y", "fn", EdgeKind::Calls);
        index.add_edge("y", "fn", "z", "fn", EdgeKind::Calls);
        index.add_edge("z", "fn", "x", "fn", EdgeKind::Calls);

        let rings = index.detect_cycles(None);
        assert_eq!(rings.len(), 2);
        let sizes: std::collections::HashSet<usize> = rings.iter().map(|r| r.len()).collect();
        assert!(sizes.contains(&2), "should have a 2-node ring");
        assert!(sizes.contains(&3), "should have a 3-node ring");
    }

    #[test]
    fn test_detect_cycles_edge_type_filter() {
        // a -Calls-> b -Calls-> a   (cycle via Calls)
        // x -DependsOn-> y           (no cycle)
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "a", "fn", EdgeKind::Calls);
        index.add_edge("x", "fn", "y", "fn", EdgeKind::DependsOn);

        // Filtering to only DependsOn: no cycles
        let rings = index.detect_cycles(Some(&[EdgeKind::DependsOn]));
        assert!(rings.is_empty());

        // Default filter (Calls + DependsOn): 1 cycle
        let rings2 = index.detect_cycles(None);
        assert_eq!(rings2.len(), 1);
    }

    #[test]
    fn test_cycle_for_node_in_ring() {
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "a", "fn", EdgeKind::Calls);

        let ring = index.cycle_for_node("b", None);
        assert!(ring.is_some());
        let ring = ring.unwrap();
        assert_eq!(ring.len(), 3);
    }

    #[test]
    fn test_cycle_for_node_not_in_ring() {
        let mut index = GraphIndex::new();
        // a -> b -> c -> a (ring), d -> e (no ring)
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "a", "fn", EdgeKind::Calls);
        index.add_edge("d", "fn", "e", "fn", EdgeKind::Calls);

        assert!(index.cycle_for_node("d", None).is_none());
        assert!(index.cycle_for_node("e", None).is_none());
    }

    #[test]
    fn test_cycle_for_node_nonexistent() {
        let index = GraphIndex::new();
        assert!(index.cycle_for_node("ghost", None).is_none());
    }

    // ── shortest_path tests ──────────────────────────────────────────────

    #[test]
    fn test_shortest_path_direct() {
        // a -> b
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);

        let path = index.shortest_path("a", "b", None);
        assert_eq!(path, Some(vec!["b".to_string()]));
    }

    #[test]
    fn test_shortest_path_multi_hop() {
        // a -> b -> c -> d
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "d", "fn", EdgeKind::Calls);

        let path = index.shortest_path("a", "d", None);
        assert_eq!(path, Some(vec!["b".to_string(), "c".to_string(), "d".to_string()]));
    }

    #[test]
    fn test_shortest_path_takes_shorter_route() {
        // a -> b -> c  (long route)
        // a -> c       (shortcut)
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("a", "fn", "c", "fn", EdgeKind::Calls);

        let path = index.shortest_path("a", "c", None);
        // Shortest path is a -> c (1 hop)
        assert_eq!(path, Some(vec!["c".to_string()]));
    }

    #[test]
    fn test_shortest_path_no_path() {
        // a -> b, x -> y (disconnected)
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("x", "fn", "y", "fn", EdgeKind::Calls);

        assert!(index.shortest_path("a", "y", None).is_none());
        assert!(index.shortest_path("b", "a", None).is_none()); // wrong direction
    }

    #[test]
    fn test_shortest_path_same_node() {
        let mut index = GraphIndex::new();
        index.ensure_node("a", "fn");

        let path = index.shortest_path("a", "a", None);
        assert_eq!(path, Some(vec![]));
    }

    #[test]
    fn test_shortest_path_nonexistent_node() {
        let index = GraphIndex::new();
        assert!(index.shortest_path("ghost1", "ghost2", None).is_none());
    }

    #[test]
    fn test_shortest_path_edge_type_filter() {
        // a -Calls-> b -Calls-> c
        // a -DependsOn-> c  (shortcut, different edge type)
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("a", "fn", "c", "fn", EdgeKind::DependsOn);

        // Only Calls: 2-hop path a -> b -> c
        let path = index.shortest_path("a", "c", Some(&[EdgeKind::Calls]));
        assert_eq!(path, Some(vec!["b".to_string(), "c".to_string()]));

        // Only DependsOn: direct a -> c
        let path2 = index.shortest_path("a", "c", Some(&[EdgeKind::DependsOn]));
        assert_eq!(path2, Some(vec!["c".to_string()]));
    }

    #[test]
    fn test_shortest_path_through_cycle() {
        // a -> b -> c -> a (cycle), but also b -> d
        // shortest from a to d: a -> b -> d
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::Calls);
        index.add_edge("c", "fn", "a", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "d", "fn", EdgeKind::Calls);

        let path = index.shortest_path("a", "d", None);
        assert_eq!(path, Some(vec!["b".to_string(), "d".to_string()]));
    }

    // ── Adversarial tests seeded from dissent ────────────────────────────
    // Dissent finding: "no cycles" when LSP hasn't run (graph has only DependsOn
    // edges from static analysis but no Calls edges). detect_cycles defaults to
    // Calls+DependsOn filter so this should still work.

    #[test]
    fn test_detect_cycles_empty_graph_returns_empty() {
        // Pre-mortem: graph with no nodes/edges → tarjan_scc should not panic.
        let index = GraphIndex::new();
        let rings = index.detect_cycles(None);
        assert!(rings.is_empty());
    }

    #[test]
    fn test_detect_cycles_calls_only_no_depends_on() {
        // Dissent: default filter is Calls+DependsOn. If a cycle only exists via
        // DependsOn and we filter to Calls-only, it must not appear.
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::DependsOn);
        index.add_edge("b", "fn", "a", "fn", EdgeKind::DependsOn);

        // Default filter (Calls+DependsOn): cycle found
        let rings_default = index.detect_cycles(None);
        assert_eq!(rings_default.len(), 1);

        // Calls-only filter: no cycle
        let rings_calls = index.detect_cycles(Some(&[EdgeKind::Calls]));
        assert!(rings_calls.is_empty());
    }

    #[test]
    fn test_detect_cycles_large_ring_all_nodes_present() {
        // Dissent: large ring output. Verify all 20 nodes are in the ring.
        let n = 20;
        let mut index = GraphIndex::new();
        for i in 0..n {
            let from = format!("n{}", i);
            let to   = format!("n{}", (i + 1) % n);
            index.add_edge(&from, "fn", &to, "fn", EdgeKind::Calls);
        }

        let rings = index.detect_cycles(None);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), n);
        for i in 0..n {
            assert!(rings[0].contains(&format!("n{}", i)), "n{} missing from ring", i);
        }
    }

    #[test]
    fn test_cycle_for_node_with_multiple_rings_returns_correct_one() {
        // Adversarial: two rings, query a node in the second. Must return the
        // correct ring, not the first one.
        let mut index = GraphIndex::new();
        // Ring 1: a -> b -> a
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "a", "fn", EdgeKind::Calls);
        // Ring 2: x -> y -> z -> x
        index.add_edge("x", "fn", "y", "fn", EdgeKind::Calls);
        index.add_edge("y", "fn", "z", "fn", EdgeKind::Calls);
        index.add_edge("z", "fn", "x", "fn", EdgeKind::Calls);

        let ring = index.cycle_for_node("z", None).expect("z should be in a ring");
        assert_eq!(ring.len(), 3);
        assert!(ring.contains(&"x".to_string()));
        assert!(ring.contains(&"y".to_string()));
        assert!(ring.contains(&"z".to_string()));
        assert!(!ring.contains(&"a".to_string()));
    }

    #[test]
    fn test_shortest_path_reverse_edge_is_none() {
        // Dissent: directed edges — caller asking for path in wrong direction.
        // a -> b exists; b -> a should be None.
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);

        assert!(index.shortest_path("b", "a", None).is_none());
    }

    #[test]
    fn test_shortest_path_unrelated_depends_on_not_followed() {
        // Dissent: default Calls-only filter. DependsOn edges are NOT followed.
        // a -Calls-> b, b -DependsOn-> c. From a to c: no path via Calls only.
        let mut index = GraphIndex::new();
        index.add_edge("a", "fn", "b", "fn", EdgeKind::Calls);
        index.add_edge("b", "fn", "c", "fn", EdgeKind::DependsOn);

        // Default (Calls only): no path
        assert!(index.shortest_path("a", "c", None).is_none());

        // With DependsOn included: path exists
        let path = index.shortest_path("a", "c", Some(&[EdgeKind::Calls, EdgeKind::DependsOn]));
        assert_eq!(path, Some(vec!["b".to_string(), "c".to_string()]));
    }
}
