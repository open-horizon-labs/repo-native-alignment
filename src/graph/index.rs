//! In-memory petgraph index for structural graph traversal.
//!
//! The `GraphIndex` is a derived index rebuilt from LanceDB edge data.
//! It provides fast BFS/DFS traversal, neighbor queries, and impact
//! analysis that would be expensive as columnar joins.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

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
        // Step 1: Build coupling subgraph as an undirected adjacency list.
        // We work with NodeIndex values directly for performance.
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

        // Collect coupling edges as undirected pairs with weights.
        // adj[node_index] = vec of (neighbor_index, weight)
        let n = self.graph.node_count();
        if n == 0 {
            return Vec::new();
        }

        let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        let mut total_weight = 0.0;

        for edge_ref in self.graph.edge_references() {
            let kind = &edge_ref.weight().edge_type;
            if !coupling_kinds.contains(kind) {
                continue;
            }
            let w = edge_weight(kind);
            let s = edge_ref.source().index();
            let t = edge_ref.target().index();
            if s == t {
                continue;
            }
            adj[s].push((t, w));
            adj[t].push((s, w));
            total_weight += w;
        }

        if total_weight == 0.0 {
            return Vec::new();
        }

        // Guard: require a minimum density of coupling edges relative to node
        // count. Without enough edges (e.g., no LSP data), the graph is too
        // sparse for Louvain to find meaningful communities. We require at
        // least 5% of nodes to have coupling edges for large graphs.
        let nodes_with_edges = adj.iter().filter(|nbrs| !nbrs.is_empty()).count();
        if nodes_with_edges < 3 || (n > 100 && (nodes_with_edges as f64) < (n as f64 * 0.05)) {
            return Vec::new();
        }

        // Weighted degree of each node (sum of coupling edge weights, undirected)
        let k: Vec<f64> = adj.iter().map(|nbrs| nbrs.iter().map(|(_, w)| w).sum()).collect();

        // Step 2: Louvain phase 1 -- iteratively move nodes to maximize modularity.
        // community[i] = community assignment of node i
        let mut community: Vec<usize> = (0..n).collect();
        let m2 = 2.0 * total_weight; // sum of all edge weights (each counted once)

        // Maintain sigma_tot incrementally: sum of weighted degrees per community.
        // Initial state: each node is its own community, so sigma_tot[i] = k[i].
        let mut sigma_tot: HashMap<usize, f64> = (0..n).map(|i| (i, k[i])).collect();

        let mut improved = true;
        let mut iterations = 0;
        while improved && iterations < 20 {
            improved = false;
            iterations += 1;

            for node in 0..n {
                if k[node] == 0.0 {
                    continue; // isolated node
                }

                let current_comm = community[node];

                // Compute weight of edges from `node` to each neighboring community
                let mut comm_weights: HashMap<usize, f64> = HashMap::new();
                for &(nbr, w) in &adj[node] {
                    *comm_weights.entry(community[nbr]).or_default() += w;
                }

                // Weight to current community (needed for removal delta)
                let w_to_current = comm_weights.get(&current_comm).copied().unwrap_or(0.0);

                let sigma_current = sigma_tot.get(&current_comm).copied().unwrap_or(0.0);
                let ki = k[node];

                // Modularity gain from removing node from its current community
                let remove_delta = -w_to_current / m2 + ki * (sigma_current - ki) / (m2 * m2);

                // Find best community to move to
                let mut best_comm = current_comm;
                let mut best_gain = 0.0;

                for (&candidate_comm, &w_to_candidate) in &comm_weights {
                    if candidate_comm == current_comm {
                        continue;
                    }
                    let sigma_candidate = sigma_tot.get(&candidate_comm).copied().unwrap_or(0.0);
                    let insert_delta = w_to_candidate / m2 - ki * sigma_candidate / (m2 * m2);
                    let gain = remove_delta + insert_delta;
                    if gain > best_gain {
                        best_gain = gain;
                        best_comm = candidate_comm;
                    }
                }

                if best_comm != current_comm {
                    // Update sigma_tot: remove node's degree from old, add to new
                    *sigma_tot.entry(current_comm).or_default() -= ki;
                    *sigma_tot.entry(best_comm).or_default() += ki;
                    community[node] = best_comm;
                    improved = true;
                }
            }
        }

        // Step 3: Collect communities, compute stats, and build Subsystem structs.
        // Group nodes by community
        let mut comm_members: HashMap<usize, Vec<usize>> = HashMap::new();
        for (node_idx, &comm) in community.iter().enumerate() {
            comm_members.entry(comm).or_default().push(node_idx);
        }

        // Filter out tiny clusters -- merge into nearest large cluster.
        // Scale threshold: for small graphs 3 is fine, for larger graphs require
        // at least 5 members per cluster to avoid noise from test helper groups.
        let min_cluster_size = if n < 50 { 3 } else { 5 };
        let large_comms: HashSet<usize> = comm_members
            .iter()
            .filter(|(_, members)| members.len() >= min_cluster_size)
            .map(|(&comm, _)| comm)
            .collect();

        if large_comms.is_empty() {
            return Vec::new();
        }

        // Reassign small-cluster nodes to the large cluster they have the most edges to
        for node in 0..n {
            if large_comms.contains(&community[node]) {
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

        // Rebuild community members after merging
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

            // Collect member node IDs
            let member_ids: Vec<String> = members
                .iter()
                .filter_map(|&idx| {
                    self.graph
                        .node_weight(NodeIndex::new(idx))
                        .map(|nr| nr.id.clone())
                })
                .collect();

            // Cluster naming: dominant file-path prefix
            let name = compute_cluster_name(&member_ids, node_file_map);

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
            });
        }

        // Sort by symbol count descending
        subsystems.sort_by(|a, b| b.symbol_count.cmp(&a.symbol_count));
        subsystems
    }
}

// ---------------------------------------------------------------------------
// Subsystem detection types
// ---------------------------------------------------------------------------

/// A detected subsystem (community) in the code graph.
#[derive(Debug, Clone)]
pub struct Subsystem {
    /// Name derived from dominant file-path prefix of members.
    pub name: String,
    /// Number of symbols in this subsystem.
    pub symbol_count: usize,
    /// Ratio of internal to total edge weight (0.0 to 1.0).
    pub cohesion: f64,
    /// Top interface functions, scored by cross_cluster_degree * pagerank.
    pub interfaces: Vec<SubsystemInterface>,
    /// Stable IDs of all member nodes in this subsystem.
    pub member_ids: Vec<String>,
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

/// Compute a cluster name from the dominant file-path prefix of member nodes.
fn compute_cluster_name(
    member_ids: &[String],
    node_file_map: &HashMap<String, String>,
) -> String {
    // Count file-path directory prefixes
    let mut prefix_counts: HashMap<&str, usize> = HashMap::new();
    for id in member_ids {
        if let Some(file_path) = node_file_map.get(id) {
            // Extract the first meaningful directory component after "src/"
            let parts: Vec<&str> = file_path.split('/').collect();
            let prefix = if parts.len() >= 2 && parts[0] == "src" {
                if parts.len() >= 3 && parts[1] != "lib.rs" && parts[1] != "main.rs" {
                    // e.g., src/graph/index.rs -> "graph"
                    parts[1]
                } else {
                    // e.g., src/server.rs -> "server" (strip .rs)
                    parts[1].strip_suffix(".rs").unwrap_or(parts[1])
                }
            } else if !parts.is_empty() {
                parts[0]
            } else {
                continue;
            };
            *prefix_counts.entry(prefix).or_default() += 1;
        }
    }

    prefix_counts
        .into_iter()
        .max_by_key(|&(_, count)| count)
        .map(|(prefix, _)| prefix.to_string())
        .unwrap_or_else(|| "unknown".to_string())
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
        // Cluster A: a1 <-> a2 <-> a3 (tightly connected via Calls)
        // Cluster B: b1 <-> b2 <-> b3 (tightly connected via Calls)
        // Single weak link between clusters: a3 -> b1
        let mut index = GraphIndex::new();
        // Cluster A: dense internal connections
        index.add_edge("a1", "fn", "a2", "fn", EdgeKind::Calls);
        index.add_edge("a2", "fn", "a1", "fn", EdgeKind::Calls);
        index.add_edge("a2", "fn", "a3", "fn", EdgeKind::Calls);
        index.add_edge("a3", "fn", "a2", "fn", EdgeKind::Calls);
        index.add_edge("a1", "fn", "a3", "fn", EdgeKind::Calls);
        index.add_edge("a3", "fn", "a1", "fn", EdgeKind::Calls);
        // Cluster B: dense internal connections
        index.add_edge("b1", "fn", "b2", "fn", EdgeKind::Calls);
        index.add_edge("b2", "fn", "b1", "fn", EdgeKind::Calls);
        index.add_edge("b2", "fn", "b3", "fn", EdgeKind::Calls);
        index.add_edge("b3", "fn", "b2", "fn", EdgeKind::Calls);
        index.add_edge("b1", "fn", "b3", "fn", EdgeKind::Calls);
        index.add_edge("b3", "fn", "b1", "fn", EdgeKind::Calls);
        // Weak inter-cluster link
        index.add_edge("a3", "fn", "b1", "fn", EdgeKind::Calls);

        let pagerank = index.compute_pagerank(0.85, 20);
        let file_map = make_file_map(&[
            ("a1", "src/alpha/mod.rs"),
            ("a2", "src/alpha/mod.rs"),
            ("a3", "src/alpha/mod.rs"),
            ("b1", "src/beta/mod.rs"),
            ("b2", "src/beta/mod.rs"),
            ("b3", "src/beta/mod.rs"),
        ]);

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
        // Same two-cluster setup; the cross-cluster link should produce an interface
        let mut index = GraphIndex::new();
        // Cluster A
        index.add_edge("a1", "fn", "a2", "fn", EdgeKind::Calls);
        index.add_edge("a2", "fn", "a1", "fn", EdgeKind::Calls);
        index.add_edge("a2", "fn", "a3", "fn", EdgeKind::Calls);
        index.add_edge("a3", "fn", "a2", "fn", EdgeKind::Calls);
        index.add_edge("a1", "fn", "a3", "fn", EdgeKind::Calls);
        index.add_edge("a3", "fn", "a1", "fn", EdgeKind::Calls);
        // Cluster B
        index.add_edge("b1", "fn", "b2", "fn", EdgeKind::Calls);
        index.add_edge("b2", "fn", "b1", "fn", EdgeKind::Calls);
        index.add_edge("b2", "fn", "b3", "fn", EdgeKind::Calls);
        index.add_edge("b3", "fn", "b2", "fn", EdgeKind::Calls);
        index.add_edge("b1", "fn", "b3", "fn", EdgeKind::Calls);
        index.add_edge("b3", "fn", "b1", "fn", EdgeKind::Calls);
        // Cross-cluster
        index.add_edge("a3", "fn", "b1", "fn", EdgeKind::Calls);

        let pagerank = index.compute_pagerank(0.85, 20);
        let file_map = make_file_map(&[
            ("a1", "src/alpha/mod.rs"),
            ("a2", "src/alpha/mod.rs"),
            ("a3", "src/alpha/mod.rs"),
            ("b1", "src/beta/mod.rs"),
            ("b2", "src/beta/mod.rs"),
            ("b3", "src/beta/mod.rs"),
        ]);

        let subsystems = index.detect_communities(&pagerank, &file_map);
        // At least one subsystem should have interface functions
        let total_interfaces: usize = subsystems.iter().map(|s| s.interfaces.len()).sum();
        assert!(
            total_interfaces > 0,
            "cross-cluster edges should produce interface functions"
        );

        // The interface nodes should be the ones on the boundary (a3 and/or b1)
        let interface_ids: Vec<&str> = subsystems
            .iter()
            .flat_map(|s| s.interfaces.iter().map(|i| i.node_id.as_str()))
            .collect();
        assert!(
            interface_ids.contains(&"a3") || interface_ids.contains(&"b1"),
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

        // Group A: connected via Calls (real coupling)
        index.add_edge("a1", "fn", "a2", "fn", EdgeKind::Calls);
        index.add_edge("a2", "fn", "a1", "fn", EdgeKind::Calls);
        index.add_edge("a2", "fn", "a3", "fn", EdgeKind::Calls);
        index.add_edge("a3", "fn", "a2", "fn", EdgeKind::Calls);
        index.add_edge("a1", "fn", "a3", "fn", EdgeKind::Calls);
        index.add_edge("a3", "fn", "a1", "fn", EdgeKind::Calls);

        // Group B: connected via Calls (real coupling)
        index.add_edge("b1", "fn", "b2", "fn", EdgeKind::Calls);
        index.add_edge("b2", "fn", "b1", "fn", EdgeKind::Calls);
        index.add_edge("b2", "fn", "b3", "fn", EdgeKind::Calls);
        index.add_edge("b3", "fn", "b2", "fn", EdgeKind::Calls);
        index.add_edge("b1", "fn", "b3", "fn", EdgeKind::Calls);
        index.add_edge("b3", "fn", "b1", "fn", EdgeKind::Calls);

        // Connect A and B ONLY via Defines edges (should be ignored for clustering)
        index.add_edge("a1", "struct", "b1", "field", EdgeKind::Defines);
        index.add_edge("a2", "struct", "b2", "field", EdgeKind::Defines);

        let pagerank = index.compute_pagerank(0.85, 20);
        let file_map = make_file_map(&[
            ("a1", "src/alpha/mod.rs"),
            ("a2", "src/alpha/mod.rs"),
            ("a3", "src/alpha/mod.rs"),
            ("b1", "src/beta/mod.rs"),
            ("b2", "src/beta/mod.rs"),
            ("b3", "src/beta/mod.rs"),
        ]);

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
    fn test_compute_cluster_name() {
        let file_map = make_file_map(&[
            ("a", "src/graph/index.rs"),
            ("b", "src/graph/mod.rs"),
            ("c", "src/graph/store.rs"),
            ("d", "src/server.rs"),
        ]);

        // Majority in src/graph/ -> name should be "graph"
        let name = compute_cluster_name(&["a".to_string(), "b".to_string(), "c".to_string()], &file_map);
        assert_eq!(name, "graph");

        // Mixed: 2 graph + 1 server -> still "graph"
        let name = compute_cluster_name(
            &["a".to_string(), "b".to_string(), "d".to_string()],
            &file_map,
        );
        assert_eq!(name, "graph");
    }

    #[test]
    fn test_detect_communities_excludes_depends_on() {
        // DependsOn edges should NOT contribute to community detection.
        // Two separate groups connected only by DependsOn should not form one community.
        let mut index = GraphIndex::new();

        // Group A: connected via Calls (real coupling)
        index.add_edge("a1", "fn", "a2", "fn", EdgeKind::Calls);
        index.add_edge("a2", "fn", "a1", "fn", EdgeKind::Calls);
        index.add_edge("a2", "fn", "a3", "fn", EdgeKind::Calls);
        index.add_edge("a3", "fn", "a2", "fn", EdgeKind::Calls);
        index.add_edge("a1", "fn", "a3", "fn", EdgeKind::Calls);
        index.add_edge("a3", "fn", "a1", "fn", EdgeKind::Calls);

        // Group B: connected via Calls (real coupling)
        index.add_edge("b1", "fn", "b2", "fn", EdgeKind::Calls);
        index.add_edge("b2", "fn", "b1", "fn", EdgeKind::Calls);
        index.add_edge("b2", "fn", "b3", "fn", EdgeKind::Calls);
        index.add_edge("b3", "fn", "b2", "fn", EdgeKind::Calls);
        index.add_edge("b1", "fn", "b3", "fn", EdgeKind::Calls);
        index.add_edge("b3", "fn", "b1", "fn", EdgeKind::Calls);

        // Connect A and B ONLY via DependsOn edges (should be ignored for clustering)
        index.add_edge("a1", "fn", "b1", "module", EdgeKind::DependsOn);
        index.add_edge("a2", "fn", "b2", "module", EdgeKind::DependsOn);

        let pagerank = index.compute_pagerank(0.85, 20);
        let file_map = make_file_map(&[
            ("a1", "src/alpha/mod.rs"),
            ("a2", "src/alpha/mod.rs"),
            ("a3", "src/alpha/mod.rs"),
            ("b1", "src/beta/mod.rs"),
            ("b2", "src/beta/mod.rs"),
            ("b3", "src/beta/mod.rs"),
        ]);

        let subsystems = index.detect_communities(&pagerank, &file_map);
        // Should detect 2 separate clusters (A and B), not merge them via DependsOn
        assert!(
            subsystems.len() >= 2,
            "Groups connected only by DependsOn edges should remain separate clusters, got {} subsystem(s)",
            subsystems.len()
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
}
