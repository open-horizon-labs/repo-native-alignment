//! Dockerfile tree-sitter extractor.
//!
//! Extracts container image dependencies and exposed ports from Dockerfiles.
//!
//! - `FROM <image>` → `dockerfile_from` node + `DependsOn` edge to base image
//! - `FROM <image> AS <alias>` → named stage with alias as node name
//! - `EXPOSE <port>` → `dockerfile_port` node
//!
//! Multi-stage builds: when a FROM references a local stage alias (e.g.,
//! `FROM builder AS runtime`), a `DependsOn` edge is emitted from the new
//! stage to the referenced stage rather than to an external image node.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

pub struct DockerfileExtractor;

impl Default for DockerfileExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl DockerfileExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for DockerfileExtractor {
    fn extensions(&self) -> &[&str] {
        // Return empty string to match extension-less files.
        // The actual filename check is done in can_handle().
        &[""]
    }

    fn can_handle(&self, path: &Path, _content: &str) -> bool {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        file_name == "Dockerfile" || file_name.starts_with("Dockerfile.")
    }

    fn name(&self) -> &str {
        "dockerfile-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_dockerfile::language())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let source = content.as_bytes();
        let root = tree.root_node();

        let mut nodes: Vec<Node> = Vec::new();
        let mut edges: Vec<Edge> = Vec::new();

        // Map stage alias → NodeId for multi-stage DependsOn edges.
        let mut stage_map: BTreeMap<String, NodeId> = BTreeMap::new();

        // Walk top-level instructions
        for i in 0..root.child_count() {
            let Some(child) = root.child(i as u32) else { continue };

            match child.kind() {
                "from_instruction" => {
                    extract_from(&child, path, source, &mut nodes, &mut edges, &mut stage_map);
                }
                "expose_instruction" => {
                    extract_expose(&child, path, source, &mut nodes);
                }
                _ => {}
            }
        }

        Ok(ExtractionResult { nodes, edges })
    }
}

/// Extract a FROM instruction.
///
/// Grammar structure:
/// ```text
/// (from_instruction
///   "FROM"
///   (image_spec
///     (image_name)        -- e.g. "nginx"
///     (image_tag)         -- e.g. ":alpine"  (optional)
///     (image_digest)      -- e.g. "@sha256:..." (optional)
///   )
///   ["AS" (image_alias)]  -- optional stage alias
/// )
/// ```
fn extract_from(
    node: &tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    stage_map: &mut BTreeMap<String, NodeId>,
) {
    // Extract image reference from image_spec child
    let image_ref = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "image_spec")
        .map(|spec| spec.utf8_text(source).unwrap_or("").trim().to_string())
        .unwrap_or_default();

    if image_ref.is_empty() {
        return;
    }

    // Extract optional stage alias from image_alias child
    let alias = node
        .children(&mut node.walk())
        .find(|c| c.kind() == "image_alias")
        .map(|a| a.utf8_text(source).unwrap_or("").trim().to_string());

    // Node name: alias if present, otherwise the image reference
    let node_name = alias.clone().unwrap_or_else(|| image_ref.clone());

    let mut metadata = BTreeMap::new();
    metadata.insert("image".to_string(), image_ref.clone());
    if let Some(ref a) = alias {
        metadata.insert("stage".to_string(), a.clone());
    }

    let node_id = NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: node_name.clone(),
        kind: NodeKind::Other("dockerfile_from".to_string()),
    };

    nodes.push(Node {
        id: node_id.clone(),
        language: "dockerfile".to_string(),
        line_start: node.start_position().row + 1,
        line_end: node.end_position().row + 1,
        signature: format!(
            "FROM {}{}",
            image_ref,
            alias
                .as_ref()
                .map(|a| format!(" AS {}", a))
                .unwrap_or_default()
        ),
        body: String::new(),
        metadata,
        source: ExtractionSource::TreeSitter,
    });

    // Register this stage in the map so later FROM stages can reference it
    if let Some(ref a) = alias {
        stage_map.insert(a.clone(), node_id.clone());
    }

    // Determine DependsOn target: local stage alias or external image node
    let (dep_target_id, dep_target_node) = if let Some(prior) = stage_map.get(&image_ref) {
        // image_ref is a previously-defined local stage alias
        (prior.clone(), None)
    } else {
        // External base image — synthesize a virtual node
        let ext_id = NodeId {
            root: "external".to_string(),
            file: Path::new("").to_path_buf(),
            name: image_ref.clone(),
            kind: NodeKind::Other("dockerfile_from".to_string()),
        };
        let ext_node = Node {
            id: ext_id.clone(),
            language: "dockerfile".to_string(),
            line_start: 0,
            line_end: 0,
            signature: format!("FROM {}", image_ref),
            body: String::new(),
            metadata: {
                let mut m = BTreeMap::new();
                m.insert("image".to_string(), image_ref.clone());
                m
            },
            source: ExtractionSource::TreeSitter,
        };
        (ext_id, Some(ext_node))
    };

    // Emit the virtual external node (only when not a local stage reference)
    if let Some(ext_node) = dep_target_node {
        nodes.push(ext_node);
    }

    // Emit DependsOn edge: current stage → base
    edges.push(Edge {
        from: node_id,
        to: dep_target_id,
        kind: EdgeKind::DependsOn,
        source: ExtractionSource::TreeSitter,
        confidence: Confidence::Detected,
    });
}

/// Extract an EXPOSE instruction.
///
/// Grammar structure:
/// ```text
/// (expose_instruction
///   "EXPOSE"
///   (expose_port)+
/// )
/// ```
fn extract_expose(
    node: &tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    for port_node in node.children(&mut node.walk()) {
        if port_node.kind() != "expose_port" {
            continue;
        }
        let port = port_node
            .utf8_text(source)
            .unwrap_or("")
            .trim()
            .to_string();
        if port.is_empty() {
            continue;
        }

        let mut metadata = BTreeMap::new();
        metadata.insert("port".to_string(), port.clone());

        nodes.push(Node {
            id: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: port.clone(),
                kind: NodeKind::Other("dockerfile_port".to_string()),
            },
            language: "dockerfile".to_string(),
            line_start: node.start_position().row + 1,
            line_end: node.end_position().row + 1,
            signature: format!("EXPOSE {}", port),
            body: String::new(),
            metadata,
            source: ExtractionSource::TreeSitter,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn extract(content: &str) -> ExtractionResult {
        let extractor = DockerfileExtractor::new();
        extractor
            .extract(Path::new("Dockerfile"), content)
            .expect("extraction failed")
    }

    #[test]
    fn test_can_handle() {
        let ext = DockerfileExtractor::new();
        assert!(ext.can_handle(Path::new("Dockerfile"), ""));
        assert!(ext.can_handle(Path::new("Dockerfile.prod"), ""));
        assert!(ext.can_handle(Path::new("Dockerfile.dev"), ""));
        assert!(!ext.can_handle(Path::new("docker-compose.yml"), ""));
        assert!(!ext.can_handle(Path::new("main.rs"), ""));
    }

    #[test]
    fn test_simple_from() {
        let result = extract("FROM nginx:alpine\n");
        assert_eq!(result.nodes.len(), 2); // stage node + external image node
        let stage = &result.nodes[0];
        assert_eq!(stage.id.name, "nginx:alpine");
        assert_eq!(stage.id.kind, NodeKind::Other("dockerfile_from".to_string()));
        assert_eq!(stage.language, "dockerfile");
        assert_eq!(stage.metadata["image"], "nginx:alpine");
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].kind, EdgeKind::DependsOn);
    }

    #[test]
    fn test_from_with_alias() {
        let result = extract("FROM rust:1.70 AS builder\n");
        let stage = &result.nodes[0];
        assert_eq!(stage.id.name, "builder");
        assert_eq!(stage.metadata["image"], "rust:1.70");
        assert_eq!(stage.metadata["stage"], "builder");
        assert_eq!(stage.signature, "FROM rust:1.70 AS builder");
    }

    #[test]
    fn test_multistage_depends_on() {
        let content = "FROM rust:1.70 AS builder\nFROM debian:slim AS runtime\nFROM builder AS final\n";
        let result = extract(content);
        // Nodes: builder stage, rust:1.70 ext, runtime stage, debian:slim ext, final stage
        // (builder is already in stage_map, so no new ext node for final->builder)
        let from_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("dockerfile_from".to_string()))
            .collect();
        assert!(from_nodes.len() >= 3, "Expected at least 3 FROM nodes");

        // final stage should have DependsOn edge to the builder stage (local, not external)
        let final_node = result
            .nodes
            .iter()
            .find(|n| n.id.name == "final")
            .expect("final stage node missing");
        let dep_edge = result
            .edges
            .iter()
            .find(|e| e.from.name == "final")
            .expect("edge from final missing");
        assert_eq!(dep_edge.to.name, "builder");
        // target should be the local stage (root = ""), not external (root = "external")
        assert_eq!(dep_edge.to.root, "", "final->builder edge should point to local stage");
        let _ = final_node;
    }

    #[test]
    fn test_expose() {
        let result = extract("FROM nginx\nEXPOSE 80\nEXPOSE 443/tcp\n");
        let port_nodes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("dockerfile_port".to_string()))
            .collect();
        assert_eq!(port_nodes.len(), 2);
        let port_names: Vec<_> = port_nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(port_names.contains(&"80"), "Expected port 80");
        assert!(port_names.contains(&"443/tcp"), "Expected port 443/tcp");
    }

    #[test]
    fn test_no_endpoints_no_edges_on_empty() {
        let result = extract("# Just a comment\n");
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
    }
}
