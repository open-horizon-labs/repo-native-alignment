//! Post-community-detection pass that promotes subsystem clusters to first-class
//! `NodeKind::Other("subsystem")` nodes with `BelongsTo` edges from member symbols.
//!
//! # Problem
//!
//! Previously, Louvain community detection stored results only as
//! `node.metadata["subsystem"] = "embed"`. Subsystems were filterable (via `--subsystem`)
//! but not traversable — you could not ask "what does subsystem A depend on?" as a graph
//! traversal, because there was no subsystem node to traverse from.
//!
//! # Solution
//!
//! [`subsystem_node_pass`] runs after `detect_communities()` and produces:
//!
//! - One `NodeKind::Other("subsystem")` node per detected subsystem, with a stable ID of
//!   `subsystem:<name>` anchored to a virtual file path.
//! - One `EdgeKind::BelongsTo` edge from each member symbol → subsystem node.
//!
//! Existing `node.metadata["subsystem"]` writes are kept for backward compatibility with
//! the `--subsystem` filter in search queries.
//!
//! # Consistent with
//!
//! - Module nodes: `NodeKind::Module` + `BelongsTo` edges (`directory_module.rs`)
//! - Package nodes: `NodeKind::Other("package")` + `DependsOn` edges (`manifest.rs`)
//! - Framework nodes: `NodeKind::Other("framework")` + `UsesFramework` edges (#469)
//!
//! # Placement
//!
//! Call this **after `detect_communities()` has run** and returned a `Vec<Subsystem>`,
//! and **before the LanceDB persist** so the nodes survive reload.
//! Called from both `build_full_graph_inner` (step 6b) and `update_graph_with_scan`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::graph::index::Subsystem;
use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// Public return type
// ---------------------------------------------------------------------------

/// Nodes and edges emitted by the subsystem-node pass.
pub struct SubsystemNodeResult {
    /// Virtual `NodeKind::Other("subsystem")` nodes, one per detected subsystem.
    pub nodes: Vec<Node>,
    /// `BelongsTo` edges: member symbol → subsystem node.
    pub edges: Vec<Edge>,
}

// Metadata key constants for subsystem nodes.
const META_SYMBOL_COUNT: &str = "symbol_count";
const META_COHESION: &str = "cohesion";
const META_INTERFACES: &str = "interfaces";

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Post-community-detection pass: emit first-class subsystem nodes and
/// `BelongsTo` edges from each member symbol to its subsystem node.
///
/// # Arguments
///
/// * `subsystems` — detected subsystems from `GraphIndex::detect_communities()`.
/// * `all_nodes` — the complete merged node list (all roots), used to resolve
///   member stable IDs back to `NodeId` for edge construction.
/// * `root_id` — the workspace root ID to anchor subsystem nodes.
///
/// # Returns
///
/// A [`SubsystemNodeResult`] containing new virtual subsystem nodes and
/// the edges linking member symbols to them. The caller must extend
/// `all_nodes` with `result.nodes` **and** `all_edges` with `result.edges`.
pub fn subsystem_node_pass(
    subsystems: &[Subsystem],
    all_nodes: &[Node],
    root_id: &str,
) -> SubsystemNodeResult {
    if subsystems.is_empty() {
        return SubsystemNodeResult {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
    }

    // Build a stable_id → NodeId index for O(1) member lookup.
    let id_index: HashMap<String, &NodeId> =
        all_nodes.iter().map(|n| (n.stable_id(), &n.id)).collect();

    let mut result_nodes: Vec<Node> = Vec::new();
    let mut result_edges: Vec<Edge> = Vec::new();

    for subsystem in subsystems {
        let subsystem_node_id = NodeId {
            root: root_id.to_string(),
            // Virtual path: subsystems/<name> — gives each subsystem a unique stable ID.
            file: PathBuf::from(format!("subsystems/{}", subsystem.name)),
            name: subsystem.name.clone(),
            kind: NodeKind::Other("subsystem".to_string()),
        };

        let mut metadata = BTreeMap::new();
        metadata.insert(
            META_SYMBOL_COUNT.to_owned(),
            subsystem.symbol_count.to_string(),
        );
        metadata.insert(
            META_COHESION.to_owned(),
            format!("{:.4}", subsystem.cohesion),
        );
        // Store interface names for display (comma-separated, top 3).
        let iface_names: Vec<String> = subsystem
            .interfaces
            .iter()
            .take(3)
            .map(|i| {
                // Extract just the symbol name (last component of stable_id).
                i.node_id
                    .split(':')
                    .rev()
                    .nth(1)
                    .unwrap_or(&i.node_id)
                    .to_string()
            })
            .collect();
        if !iface_names.is_empty() {
            metadata.insert(META_INTERFACES.to_owned(), iface_names.join(", "));
        }

        let subsystem_node = Node {
            id: subsystem_node_id.clone(),
            language: String::new(),
            line_start: 0,
            line_end: 0,
            signature: format!("subsystem {}", subsystem.name),
            body: String::new(),
            metadata,
            source: ExtractionSource::TreeSitter,
        };
        result_nodes.push(subsystem_node);

        // Emit BelongsTo edges: member → subsystem node.
        for member_id in &subsystem.member_ids {
            if let Some(member_node_id) = id_index.get(member_id) {
                // Skip subsystem nodes themselves — no self-loops.
                if matches!(&member_node_id.kind, NodeKind::Other(s) if s == "subsystem") {
                    continue;
                }
                result_edges.push(Edge {
                    from: (*member_node_id).clone(),
                    to: subsystem_node_id.clone(),
                    kind: EdgeKind::BelongsTo,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }
        }
    }

    if !result_edges.is_empty() {
        tracing::info!(
            "Subsystem node pass: {} subsystem node(s), {} BelongsTo edge(s)",
            result_nodes.len(),
            result_edges.len(),
        );
    }

    SubsystemNodeResult {
        nodes: result_nodes,
        edges: result_edges,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::graph::index::{Subsystem, SubsystemInterface};
    use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

    fn make_node(root: &str, file: &str, name: &str) -> Node {
        Node {
            id: NodeId {
                root: root.into(),
                file: PathBuf::from(file),
                name: name.into(),
                kind: NodeKind::Function,
            },
            language: "rust".into(),
            line_start: 1,
            line_end: 10,
            signature: format!("fn {}()", name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_subsystem(name: &str, members: &[&Node]) -> Subsystem {
        Subsystem {
            name: name.to_string(),
            symbol_count: members.len(),
            cohesion: 0.8,
            interfaces: vec![SubsystemInterface {
                node_id: members.first().map(|n| n.stable_id()).unwrap_or_default(),
                node_type: "function".to_string(),
                interface_score: 1.0,
            }],
            member_ids: members.iter().map(|n| n.stable_id()).collect(),
            children: Vec::new(),
        }
    }

    #[test]
    fn test_emits_subsystem_nodes_and_edges() {
        let node_a = make_node("repo", "src/embed/mod.rs", "embed_fn");
        let node_b = make_node("repo", "src/embed/index.rs", "index_fn");
        let subsystem = make_subsystem("embed", &[&node_a, &node_b]);

        let all_nodes = vec![node_a.clone(), node_b.clone()];
        let result = subsystem_node_pass(&[subsystem], &all_nodes, "repo");

        assert_eq!(result.nodes.len(), 1, "one subsystem node");
        assert_eq!(result.edges.len(), 2, "two BelongsTo edges");

        let sub_node = &result.nodes[0];
        assert_eq!(sub_node.id.name, "embed");
        assert!(matches!(&sub_node.id.kind, NodeKind::Other(s) if s == "subsystem"));
        assert_eq!(sub_node.id.file, PathBuf::from("subsystems/embed"));

        for edge in &result.edges {
            assert_eq!(edge.kind, EdgeKind::BelongsTo);
            assert_eq!(edge.to.name, "embed");
            assert!(matches!(&edge.to.kind, NodeKind::Other(s) if s == "subsystem"));
        }
    }

    #[test]
    fn test_empty_subsystems_returns_empty() {
        let node = make_node("repo", "src/lib.rs", "foo");
        let result = subsystem_node_pass(&[], &[node], "repo");
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_unknown_member_ids_are_skipped() {
        // Member ID not in all_nodes — should not panic, should skip gracefully.
        let subsystem = Subsystem {
            name: "ghost".to_string(),
            symbol_count: 1,
            cohesion: 0.5,
            interfaces: Vec::new(),
            member_ids: vec!["ghost:nonexistent:fn:function".to_string()],
            children: Vec::new(),
        };
        let all_nodes: Vec<Node> = vec![];
        let result = subsystem_node_pass(&[subsystem], &all_nodes, "repo");
        // Subsystem node emitted but no edges (member not found).
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.edges.len(), 0);
    }

    #[test]
    fn test_metadata_populated() {
        let node_a = make_node("repo", "src/server/mod.rs", "handle");
        let node_b = make_node("repo", "src/server/state.rs", "state_fn");
        let subsystem = make_subsystem("server", &[&node_a, &node_b]);

        let all_nodes = vec![node_a, node_b];
        let result = subsystem_node_pass(&[subsystem], &all_nodes, "repo");

        let sub_node = &result.nodes[0];
        assert!(sub_node.metadata.contains_key(META_SYMBOL_COUNT));
        assert!(sub_node.metadata.contains_key(META_COHESION));
        assert!(sub_node.metadata.contains_key(META_INTERFACES));
        assert_eq!(
            sub_node.metadata.get(META_SYMBOL_COUNT).map(|s| s.as_str()),
            Some("2")
        );
    }

    #[test]
    fn test_multiple_subsystems() {
        let node_a = make_node("repo", "src/embed/mod.rs", "embed_fn");
        let node_b = make_node("repo", "src/server/mod.rs", "handle");
        let sub_embed = make_subsystem("embed", &[&node_a]);
        let sub_server = make_subsystem("server", &[&node_b]);

        let all_nodes = vec![node_a, node_b];
        let result = subsystem_node_pass(&[sub_embed, sub_server], &all_nodes, "repo");

        assert_eq!(result.nodes.len(), 2, "two subsystem nodes");
        assert_eq!(result.edges.len(), 2, "two BelongsTo edges");

        let names: std::collections::HashSet<&str> =
            result.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains("embed"));
        assert!(names.contains("server"));
    }

    #[test]
    fn test_subsystem_nodes_not_members_of_themselves() {
        // If a subsystem node were somehow in all_nodes (e.g., from a reload),
        // it must not emit a self-loop BelongsTo edge.
        let member = make_node("repo", "src/embed/mod.rs", "embed_fn");
        let mut sub_node_as_member = make_node("repo", "subsystems/embed", "embed");
        sub_node_as_member.id.kind = NodeKind::Other("subsystem".to_string());

        let subsystem = make_subsystem("embed", &[&member, &sub_node_as_member]);
        let all_nodes = vec![member, sub_node_as_member];
        let result = subsystem_node_pass(&[subsystem], &all_nodes, "repo");

        // Only the real member should get an edge, not the subsystem node.
        assert_eq!(
            result.edges.len(),
            1,
            "subsystem node must not get BelongsTo edge"
        );
        assert_ne!(result.edges[0].from.name, "embed");
    }
}
