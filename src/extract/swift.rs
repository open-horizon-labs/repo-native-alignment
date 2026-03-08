//! Swift tree-sitter extractor.
//!
//! Extracts functions, classes/structs/enums/actors (via class_declaration with declaration_kind),
//! protocols, and import declarations from `.swift` files.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::string_literals::harvest_string_literals;
use super::{ExtractionResult, Extractor};

pub struct SwiftExtractor;

impl SwiftExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for SwiftExtractor {
    fn extensions(&self) -> &[&str] {
        &["swift"]
    }

    fn name(&self) -> &str {
        "swift-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_swift::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let source = content.as_bytes();

        collect_nodes(tree.root_node(), path, source, None, &mut nodes, &mut edges);

        // Harvest string literals as synthetic Const nodes
        harvest_string_literals(
            tree.root_node(),
            path,
            source,
            "swift",
            "string_literal",
            Some("string_literal_segment"),
            &mut nodes,
        );

        Ok(ExtractionResult { nodes, edges })
    }
}

fn collect_nodes(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    class_scope: Option<&str>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    match kind_str {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let qualified = match class_scope {
                    Some(cls) => format!("{}.{}", cls, name),
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
                    language: "swift".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body,
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        "class_declaration" => {
            // Swift uses class_declaration for class, struct, enum, actor, extension
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
                    language: "swift".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body: String::new(),
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });

                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        collect_nodes(child, path, source, Some(&name), nodes, edges);
                    }
                }
                return;
            }
        }
        "protocol_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let sig = node.utf8_text(source).unwrap_or("").lines().next().unwrap_or("").trim().to_string();

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name.clone(),
                        kind: NodeKind::Trait,
                    },
                    language: "swift".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body: String::new(),
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });

                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        collect_nodes(child, path, source, Some(&name), nodes, edges);
                    }
                }
                return;
            }
        }
        "property_declaration" => {
            // Swift module-level `let` bindings are constants
            let decl_text = node.utf8_text(source).unwrap_or("").to_string();
            if decl_text.trim_start().starts_with("let ") && class_scope.is_none() {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node.utf8_text(source).unwrap_or("unknown").trim().to_string();
                    let sig = decl_text.lines().next().unwrap_or("").trim().to_string();
                    let value_str = decl_text.find('=')
                        .map(|pos| decl_text[pos+1..].trim().to_string())
                        .filter(|s| !s.is_empty());
                    let mut metadata = BTreeMap::new();
                    if let Some(ref v) = value_str {
                        let is_scalar = v.starts_with('"') || v.parse::<f64>().is_ok()
                            || v == "true" || v == "false";
                        if is_scalar {
                            let stripped = v.trim_matches('"');
                            metadata.insert("value".to_string(), stripped.to_string());
                        }
                    }
                    metadata.insert("synthetic".to_string(), "false".to_string());
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name,
                            kind: NodeKind::Const,
                        },
                        language: "swift".to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        signature: sig,
                        body: decl_text,
                        metadata,
                        source: ExtractionSource::TreeSitter,
                    });
                    return;
                }
            }
        }
        "import_declaration" => {
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();
            // "import Foundation" -> "Foundation"
            let target = text
                .split_whitespace()
                .last()
                .unwrap_or("")
                .to_string();

            let import_node = Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: text.clone(),
                    kind: NodeKind::Import,
                },
                language: "swift".to_string(),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                signature: text,
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            };

            if !target.is_empty() && target != "import" {
                edges.push(Edge {
                    from: import_node.id.clone(),
                    to: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: target,
                        kind: NodeKind::Module,
                    },
                    kind: EdgeKind::DependsOn,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }

            nodes.push(import_node);
            return;
        }
        _ => {}
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_nodes(child, path, source, class_scope, nodes, edges);
        }
    }
}
