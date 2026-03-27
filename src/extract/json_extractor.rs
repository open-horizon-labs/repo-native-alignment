//! JSON tree-sitter extractor.
//!
//! Extracts top-level keys from JSON objects. Useful for package.json (scripts,
//! dependencies), tsconfig.json, openapi specs, and similar structured configs.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

pub struct JsonExtractor;

impl Default for JsonExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for JsonExtractor {
    fn extensions(&self) -> &[&str] {
        &["json", "jsonc"]
    }

    fn name(&self) -> &str {
        "json-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_json::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let source = content.as_bytes();

        // Walk: document → object → pair (top-level only)
        let root = tree.root_node();
        for i in 0..root.child_count() {
            let Some(child) = root.child(i as u32) else {
                continue;
            };
            if child.kind() == "object" {
                extract_top_level_keys(child, path, source, &mut nodes);
                break;
            }
        }

        Ok(ExtractionResult {
            nodes,
            edges: Vec::new(),
        })
    }
}

fn extract_top_level_keys(
    object: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    for i in 0..object.child_count() {
        let Some(child) = object.child(i as u32) else {
            continue;
        };
        if child.kind() != "pair" {
            continue;
        }

        let Some(key_node) = child.child_by_field_name("key") else {
            continue;
        };
        let key = key_node
            .utf8_text(source)
            .unwrap_or("")
            .trim_matches('"')
            .to_string();

        if key.is_empty() {
            continue;
        }

        let value_text = child
            .child_by_field_name("value")
            .and_then(|v| v.utf8_text(source).ok())
            .unwrap_or("")
            .to_string();

        // Check if value is a scalar (string, number, boolean, null — not object/array)
        let val_node_opt = child.child_by_field_name("value");
        let is_scalar = val_node_opt
            .map(|v| !matches!(v.kind(), "object" | "array"))
            .unwrap_or(false);

        // Truncate large values
        let body = if value_text.len() > 500 {
            format!("{}...", &value_text[..500])
        } else {
            value_text.clone()
        };

        let (kind, metadata) = if is_scalar && !value_text.trim().is_empty() {
            let mut m = BTreeMap::new();
            m.insert(
                "value".to_string(),
                value_text.trim().trim_matches('"').to_string(),
            );
            m.insert("synthetic".to_string(), "true".to_string());
            (NodeKind::Const, m)
        } else {
            (NodeKind::Other("json_key".to_string()), BTreeMap::new())
        };

        nodes.push(Node {
            id: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: key.clone(),
                kind,
            },
            language: "json".to_string(),
            line_start: child.start_position().row + 1,
            line_end: child.end_position().row + 1,
            signature: format!("\"{}\":", key),
            body,
            metadata,
            source: ExtractionSource::TreeSitter,
        });
    }
}
