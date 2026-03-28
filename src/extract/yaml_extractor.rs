//! YAML tree-sitter extractor.
//!
//! Extracts top-level keys and, for Kubernetes manifests, `kind` + `metadata.name`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

pub struct YamlExtractor;

impl Default for YamlExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl YamlExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for YamlExtractor {
    fn extensions(&self) -> &[&str] {
        &["yaml", "yml"]
    }

    fn name(&self) -> &str {
        "yaml-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_yaml::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let source = content.as_bytes();

        extract_documents(tree.root_node(), path, source, &mut nodes);

        Ok(ExtractionResult {
            nodes,
            edges: Vec::new(),
        })
    }
}

fn extract_documents(root: tree_sitter::Node, path: &Path, source: &[u8], nodes: &mut Vec<Node>) {
    for i in 0..root.child_count() {
        let Some(child) = root.child(i as u32) else {
            continue;
        };
        if child.kind() == "document" {
            extract_document(child, path, source, nodes);
        }
    }
}

fn extract_document(doc: tree_sitter::Node, path: &Path, source: &[u8], nodes: &mut Vec<Node>) {
    // Find the root mapping of the document
    let mapping = find_block_mapping(doc);
    let Some(mapping) = mapping else { return };

    // Try to detect Kubernetes manifest: has 'kind' and 'metadata.name'
    let kind_val = get_mapping_value(mapping, source, "kind");
    let api_version = get_mapping_value(mapping, source, "apiVersion");

    if kind_val.is_some() && api_version.is_some() {
        // Kubernetes-style manifest
        let k8s_kind = kind_val.as_deref().unwrap_or("Unknown");
        let metadata_name = get_metadata_name(mapping, source);

        let name = match &metadata_name {
            Some(n) => format!("{}/{}", k8s_kind, n),
            None => k8s_kind.to_string(),
        };

        let mut metadata = BTreeMap::new();
        if let Some(av) = &api_version {
            metadata.insert("apiVersion".to_string(), av.clone());
        }
        if let Some(n) = &metadata_name {
            metadata.insert("name".to_string(), n.clone());
        }

        nodes.push(Node {
            id: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name,
                kind: NodeKind::Other(format!("k8s_{}", k8s_kind.to_lowercase())),
            },
            language: "yaml".to_string(),
            line_start: doc.start_position().row + 1,
            line_end: doc.end_position().row + 1,
            signature: format!("{} {}", k8s_kind, metadata_name.as_deref().unwrap_or("")),
            body: String::new(),
            metadata,
            source: ExtractionSource::TreeSitter,
        });
        return;
    }

    // Generic YAML: extract top-level keys
    for i in 0..mapping.child_count() {
        let Some(pair) = mapping.child(i as u32) else {
            continue;
        };
        if pair.kind() != "block_mapping_pair" {
            continue;
        }

        let Some(key_node) = pair.child_by_field_name("key") else {
            continue;
        };
        let key = key_text(key_node, source);
        if key.is_empty() {
            continue;
        }

        let value_node = pair.child_by_field_name("value");
        let value_text = value_node
            .and_then(|v| v.utf8_text(source).ok())
            .unwrap_or("")
            .to_string();

        let body = if value_text.len() > 300 {
            format!("{}...", &value_text[..300])
        } else {
            value_text.clone()
        };

        // Check if the value is a scalar (not a mapping or sequence)
        let is_scalar = value_node
            .map(|v| {
                !matches!(
                    v.kind(),
                    "block_mapping" | "flow_mapping" | "block_sequence" | "flow_sequence"
                )
            })
            .unwrap_or(false);

        let (kind, metadata) = if is_scalar && !value_text.trim().is_empty() {
            let mut m = BTreeMap::new();
            m.insert("value".to_string(), value_text.trim().to_string());
            m.insert("synthetic".to_string(), "true".to_string());
            (NodeKind::Const, m)
        } else {
            (NodeKind::Other("yaml_key".to_string()), BTreeMap::new())
        };

        nodes.push(Node {
            id: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: key.clone(),
                kind,
            },
            language: "yaml".to_string(),
            line_start: pair.start_position().row + 1,
            line_end: pair.end_position().row + 1,
            signature: format!("{}:", key),
            body,
            metadata,
            source: ExtractionSource::TreeSitter,
        });
    }
}

fn find_block_mapping(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    if node.kind() == "block_mapping" {
        return Some(node);
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32)
            && let Some(found) = find_block_mapping(child)
        {
            return Some(found);
        }
    }
    None
}

fn get_mapping_value<'a>(
    mapping: tree_sitter::Node<'a>,
    source: &[u8],
    target_key: &str,
) -> Option<String> {
    for i in 0..mapping.child_count() {
        // Use `let..else` to skip None children (punctuation/anonymous nodes)
        // rather than `?` which would return None from the entire function.
        let Some(pair) = mapping.child(i as u32) else {
            continue;
        };
        if pair.kind() != "block_mapping_pair" {
            continue;
        }
        let Some(key_node) = pair.child_by_field_name("key") else {
            continue;
        };
        if key_text(key_node, source) == target_key {
            let val = pair
                .child_by_field_name("value")
                .and_then(|v| v.utf8_text(source).ok())
                .unwrap_or("")
                .trim()
                .to_string();
            return Some(val);
        }
    }
    None
}

fn get_metadata_name(mapping: tree_sitter::Node, source: &[u8]) -> Option<String> {
    // Find the 'metadata' key, then get 'name' from its mapping
    for i in 0..mapping.child_count() {
        let Some(pair) = mapping.child(i as u32) else {
            continue;
        };
        if pair.kind() != "block_mapping_pair" {
            continue;
        }
        let Some(key_node) = pair.child_by_field_name("key") else {
            continue;
        };
        if key_text(key_node, source) != "metadata" {
            continue;
        }
        let Some(val_node) = pair.child_by_field_name("value") else {
            continue;
        };
        if let Some(nested) = find_block_mapping(val_node) {
            return get_mapping_value(nested, source, "name");
        }
    }
    None
}

fn key_text(node: tree_sitter::Node, source: &[u8]) -> String {
    node.utf8_text(source)
        .unwrap_or("")
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_yaml_scalar_kvs_become_const() {
        let extractor = YamlExtractor::new();
        let code = "version: \"1.0.0\"\nmax_connections: 100\n";
        let result = extractor.extract(Path::new("config.yaml"), code).unwrap();
        let consts: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Const)
            .collect();
        assert!(
            !consts.is_empty(),
            "Should find Const nodes for scalar YAML values"
        );
        let c = consts
            .iter()
            .find(|n| n.id.name == "version")
            .expect("Should find version");
        assert_eq!(
            c.metadata.get("synthetic").map(|s| s.as_str()),
            Some("true"),
            "YAML scalars should be synthetic"
        );
    }
}
