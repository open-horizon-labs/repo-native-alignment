//! C# tree-sitter extractor.
//!
//! Extracts classes, interfaces, structs, enums, methods, constructors,
//! and using directives from `.cs` files.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::string_literals::harvest_string_literals;
use super::{ExtractionResult, Extractor};

pub struct CSharpExtractor;

impl CSharpExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for CSharpExtractor {
    fn extensions(&self) -> &[&str] {
        &["cs"]
    }

    fn name(&self) -> &str {
        "csharp-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_c_sharp::LANGUAGE.into())?;
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
            "csharp",
            "string_literal",
            None,
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
        "class_declaration" | "struct_declaration" => {
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
                    language: "csharp".to_string(),
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
        "interface_declaration" => {
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
                    language: "csharp".to_string(),
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
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let sig = node.utf8_text(source).unwrap_or("").lines().next().unwrap_or("").trim().to_string();

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name.clone(),
                        kind: NodeKind::Enum,
                    },
                    language: "csharp".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body: String::new(),
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let qualified = match class_scope {
                    Some(cls) => format!("{}.{}", cls, name),
                    None => name,
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
                    language: "csharp".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body,
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        "constructor_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let qualified = match class_scope {
                    Some(cls) => format!("{}..ctor", cls),
                    None => name,
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
                    language: "csharp".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body,
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        "field_declaration" => {
            // C# constants: `public const int MAX_SIZE = 1024;`
            let decl_text = node.utf8_text(source).unwrap_or("").to_string();
            if decl_text.contains("const ") {
                // Walk variable_declarator children
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        // C# field_declaration → variable_declaration → variable_declarator
                        if child.kind() == "variable_declaration" {
                            for j in 0..child.child_count() {
                                if let Some(decl) = child.child(j as u32) {
                                    if decl.kind() == "variable_declarator" {
                                        if let Some(name_node) = decl.child_by_field_name("name") {
                                            let name = name_node.utf8_text(source).unwrap_or("unknown").trim().to_string();
                                            let qualified = match class_scope {
                                                Some(cls) => format!("{}.{}", cls, name),
                                                None => name,
                                            };
                                            let sig = decl_text.lines().next().unwrap_or("").trim().to_string();
                                            // Value may be in equals_value_clause
                                            let value_str = decl.child_by_field_name("initializer")
                                                .and_then(|v| v.utf8_text(source).ok())
                                                .map(|s| s.trim_start_matches('=').trim().to_string());
                                            let mut metadata = BTreeMap::new();
                                            if let Some(ref v) = value_str {
                                                let is_scalar = v.starts_with('"') || v.starts_with('\'')
                                                    || v.parse::<f64>().is_ok()
                                                    || v == "true" || v == "false";
                                                if is_scalar {
                                                    let stripped = v.trim_matches('"').trim_matches('\'');
                                                    metadata.insert("value".to_string(), stripped.to_string());
                                                }
                                            }
                                            metadata.insert("synthetic".to_string(), "false".to_string());
                                            nodes.push(Node {
                                                id: NodeId {
                                                    root: String::new(),
                                                    file: path.to_path_buf(),
                                                    name: qualified,
                                                    kind: NodeKind::Const,
                                                },
                                                language: "csharp".to_string(),
                                                line_start: node.start_position().row + 1,
                                                line_end: node.end_position().row + 1,
                                                signature: sig,
                                                body: decl_text.clone(),
                                                metadata,
                                                source: ExtractionSource::TreeSitter,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        "using_directive" => {
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();
            let target = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();

            let import_node = Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: text.clone(),
                    kind: NodeKind::Import,
                },
                language: "csharp".to_string(),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                signature: text,
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            };

            if !target.is_empty() {
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
