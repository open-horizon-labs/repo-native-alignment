//! GDScript tree-sitter extractor.
//!
//! Generic path: functions, classes, string literals, complexity, decorators.
//! Special cases: signals, export/onready vars, class_name, extends.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::configs::GDSCRIPT_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct GDScriptExtractor;

impl Default for GDScriptExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl GDScriptExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for GDScriptExtractor {
    fn extensions(&self) -> &[&str] {
        &["gd"]
    }

    fn name(&self) -> &str {
        "gdscript-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut result = GenericExtractor::new(&GDSCRIPT_CONFIG).run(path, content)?;

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_gdscript::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            let source = content.as_bytes();
            let mut extends_type: Option<String> = None;
            let mut class_name_idx: Option<usize> = None;

            collect_gdscript_nodes(
                tree.root_node(),
                path,
                source,
                &mut result.nodes,
                &mut extends_type,
                &mut class_name_idx,
            );

            // Attach extends metadata to the class_name node if both exist.
            if let Some(idx) = class_name_idx
                && let Some(ref ext) = extends_type
            {
                result.nodes[idx]
                    .metadata
                    .insert("extends".to_string(), ext.clone());
            }
        }

        Ok(result)
    }
}

fn collect_gdscript_nodes(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    extends_type: &mut Option<String>,
    class_name_idx: &mut Option<usize>,
) {
    for i in 0..node.child_count() {
        let Some(child) = node.child(i as u32) else {
            continue;
        };

        match child.kind() {
            "signal_statement" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = name_node.utf8_text(source).unwrap_or("").to_string();
                    let signature = first_line_trimmed(&child, source);
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name,
                            kind: NodeKind::Field,
                        },
                        language: "gdscript".to_string(),
                        line_start: child.start_position().row + 1,
                        line_end: child.end_position().row + 1,
                        signature,
                        body: String::new(),
                        metadata: {
                            let mut m = BTreeMap::new();
                            m.insert("signal".to_string(), "true".to_string());
                            m
                        },
                        source: ExtractionSource::TreeSitter,
                    });
                }
            }
            "export_variable_statement" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = name_node.utf8_text(source).unwrap_or("").to_string();
                    let signature = first_line_trimmed(&child, source);
                    let mut metadata = BTreeMap::new();
                    metadata.insert("decorator".to_string(), "@export".to_string());
                    if let Some(type_node) = child.child_by_field_name("type") {
                        let t = type_node.utf8_text(source).unwrap_or("").trim().to_string();
                        if !t.is_empty() {
                            metadata.insert("type".to_string(), t);
                        }
                    }
                    if let Some(val_node) = child.child_by_field_name("value") {
                        let v = val_node.utf8_text(source).unwrap_or("").trim().to_string();
                        if !v.is_empty() {
                            metadata.insert("value".to_string(), v);
                        }
                    }
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name,
                            kind: NodeKind::Field,
                        },
                        language: "gdscript".to_string(),
                        line_start: child.start_position().row + 1,
                        line_end: child.end_position().row + 1,
                        signature,
                        body: String::new(),
                        metadata,
                        source: ExtractionSource::TreeSitter,
                    });
                }
            }
            "onready_variable_statement" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = name_node.utf8_text(source).unwrap_or("").to_string();
                    let signature = first_line_trimmed(&child, source);
                    let mut metadata = BTreeMap::new();
                    metadata.insert("decorator".to_string(), "@onready".to_string());
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name,
                            kind: NodeKind::Field,
                        },
                        language: "gdscript".to_string(),
                        line_start: child.start_position().row + 1,
                        line_end: child.end_position().row + 1,
                        signature,
                        body: String::new(),
                        metadata,
                        source: ExtractionSource::TreeSitter,
                    });
                }
            }
            "class_name_statement" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = name_node.utf8_text(source).unwrap_or("").to_string();
                    let signature = first_line_trimmed(&child, source);
                    let idx = nodes.len();
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name,
                            kind: NodeKind::Struct,
                        },
                        language: "gdscript".to_string(),
                        line_start: child.start_position().row + 1,
                        line_end: child.end_position().row + 1,
                        signature,
                        body: String::new(),
                        metadata: BTreeMap::new(),
                        source: ExtractionSource::TreeSitter,
                    });
                    *class_name_idx = Some(idx);
                }
            }
            "extends_statement" => {
                // The extends type is the text of the first non-keyword child.
                let ext_text = child
                    .utf8_text(source)
                    .unwrap_or("")
                    .trim()
                    .strip_prefix("extends")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !ext_text.is_empty() {
                    *extends_type = Some(ext_text);
                }
            }
            // Recurse into class_definition bodies to find nested export/onready vars.
            "class_definition" => {
                if let Some(body) = child.child_by_field_name("body") {
                    collect_gdscript_nodes(body, path, source, nodes, extends_type, class_name_idx);
                }
            }
            _ => {}
        }
    }
}

fn first_line_trimmed(node: &tree_sitter::Node, source: &[u8]) -> String {
    node.utf8_text(source)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}
