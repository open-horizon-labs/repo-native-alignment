//! TOML tree-sitter extractor.
//!
//! Extracts table headers and top-level key-value pairs. Useful for Cargo.toml,
//! pyproject.toml, config.toml, etc.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

pub struct TomlExtractor;

impl TomlExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for TomlExtractor {
    fn extensions(&self) -> &[&str] {
        &["toml"]
    }

    fn name(&self) -> &str {
        "toml-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_toml_ng::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let source = content.as_bytes();
        let root = tree.root_node();

        for i in 0..root.child_count() {
            let Some(child) = root.child(i as u32) else { continue };
            match child.kind() {
                "table" | "table_array_element" => {
                    // [section] or [[array_section]]
                    if let Some(key_node) = child.child(1 as u32) {
                        let name = key_node.utf8_text(source).unwrap_or("").trim().to_string();
                        if !name.is_empty() {
                            nodes.push(Node {
                                id: NodeId {
                                    root: String::new(),
                                    file: path.to_path_buf(),
                                    name: name.clone(),
                                    kind: NodeKind::Module,
                                },
                                language: "toml".to_string(),
                                line_start: child.start_position().row + 1,
                                line_end: child.end_position().row + 1,
                                signature: format!("[{}]", name),
                                body: child.utf8_text(source).unwrap_or("").to_string(),
                                metadata: BTreeMap::new(),
                                source: ExtractionSource::TreeSitter,
                            });
                        }
                    }
                }
                "pair" => {
                    // Top-level key = value
                    if let Some(key_node) = child.child_by_field_name("key") {
                        let key = key_node.utf8_text(source).unwrap_or("").trim().to_string();
                        if !key.is_empty() {
                            let val = child
                                .child_by_field_name("value")
                                .and_then(|v| v.utf8_text(source).ok())
                                .unwrap_or("")
                                .to_string();

                            nodes.push(Node {
                                id: NodeId {
                                    root: String::new(),
                                    file: path.to_path_buf(),
                                    name: key.clone(),
                                    kind: NodeKind::Other("toml_key".to_string()),
                                },
                                language: "toml".to_string(),
                                line_start: child.start_position().row + 1,
                                line_end: child.end_position().row + 1,
                                signature: format!("{} = {}", key, val),
                                body: val,
                                metadata: BTreeMap::new(),
                                source: ExtractionSource::TreeSitter,
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(ExtractionResult { nodes, edges: Vec::new() })
    }
}
