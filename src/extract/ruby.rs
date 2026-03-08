//! Ruby tree-sitter extractor.
//!
//! Extracts methods, singleton methods, classes, and modules from `.rb` files.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

pub struct RubyExtractor;

impl RubyExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for RubyExtractor {
    fn extensions(&self) -> &[&str] {
        &["rb"]
    }

    fn name(&self) -> &str {
        "ruby-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_ruby::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let source = content.as_bytes();

        collect_nodes(tree.root_node(), path, source, None, &mut nodes);

        Ok(ExtractionResult { nodes, edges: Vec::new() })
    }
}

fn collect_nodes(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    scope: Option<&str>,
    nodes: &mut Vec<Node>,
) {
    match node.kind() {
        "method" | "singleton_method" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let qualified = match scope {
                    Some(s) => format!("{}.{}", s, name),
                    None => name.clone(),
                };
                let body = node.utf8_text(source).unwrap_or("").to_string();
                let sig = body.lines().next().unwrap_or("").trim().to_string();

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: qualified,
                        kind: NodeKind::Function,
                    },
                    language: "ruby".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body,
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        "class" | "singleton_class" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let sig = node.utf8_text(source).unwrap_or("").lines().next().unwrap_or("").trim().to_string();

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name.clone(),
                        kind: NodeKind::Struct,
                    },
                    language: "ruby".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body: String::new(),
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });

                // Recurse into class body with class name as scope
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        collect_nodes(child, path, source, Some(&name), nodes);
                    }
                }
                return;
            }
        }
        "module" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let sig = node.utf8_text(source).unwrap_or("").lines().next().unwrap_or("").trim().to_string();

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name.clone(),
                        kind: NodeKind::Module,
                    },
                    language: "ruby".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body: String::new(),
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });

                // Recurse with module name as scope
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        collect_nodes(child, path, source, Some(&name), nodes);
                    }
                }
                return;
            }
        }
        _ => {}
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_nodes(child, path, source, scope, nodes);
        }
    }
}
