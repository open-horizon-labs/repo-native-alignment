//! Zig tree-sitter extractor.
//!
//! Extracts function declarations, struct declarations, and enum declarations from `.zig` files.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::string_literals::harvest_string_literals;
use super::{ExtractionResult, Extractor};

pub struct ZigExtractor;

impl ZigExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for ZigExtractor {
    fn extensions(&self) -> &[&str] {
        &["zig"]
    }

    fn name(&self) -> &str {
        "zig-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_zig::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let source = content.as_bytes();

        collect_nodes(tree.root_node(), path, source, &mut nodes);

        // Harvest string literals as synthetic Const nodes
        harvest_string_literals(
            tree.root_node(),
            path,
            source,
            "zig",
            "string_literal",
            None,
            &mut nodes,
        );

        Ok(ExtractionResult { nodes, edges: Vec::new() })
    }
}

fn collect_nodes(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    match node.kind() {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let body = node.utf8_text(source).unwrap_or("").to_string();
                let sig = body.lines().next().unwrap_or("").trim().to_string();

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name,
                        kind: NodeKind::Function,
                    },
                    language: "zig".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body,
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        "struct_declaration" => {
            // Zig structs are often assigned to a const: `const Foo = struct { ... };`
            // The struct_declaration node may not carry a name field; name comes from parent.
            // We attempt to get it from the parent's var_decl name via text heuristic.
            let body = node.utf8_text(source).unwrap_or("").to_string();
            let sig = body.lines().next().unwrap_or("").trim().to_string();

            nodes.push(Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: sig.clone(),
                    kind: NodeKind::Struct,
                },
                language: "zig".to_string(),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                signature: sig,
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            });
        }
        "enum_declaration" => {
            let body = node.utf8_text(source).unwrap_or("").to_string();
            let sig = body.lines().next().unwrap_or("").trim().to_string();

            nodes.push(Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: sig.clone(),
                    kind: NodeKind::Enum,
                },
                language: "zig".to_string(),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                signature: sig,
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            });
        }
        "variable_declaration" => {
            // Zig: `const MAX_SIZE: usize = 1024;`
            let decl_text = node.utf8_text(source).unwrap_or("").to_string();
            if decl_text.trim_start().starts_with("const ") {
                if let Some(name_node) = node.child_by_field_name("variable_name") {
                    let name = name_node.utf8_text(source).unwrap_or("unknown").trim().to_string();
                    if !name.is_empty() {
                        let sig = decl_text.lines().next().unwrap_or("").trim().to_string();
                        let value_str = decl_text.find('=')
                            .map(|pos| decl_text[pos+1..].trim_end_matches(';').trim().to_string())
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
                            language: "zig".to_string(),
                            line_start: node.start_position().row + 1,
                            line_end: node.end_position().row + 1,
                            signature: sig,
                            body: decl_text,
                            metadata,
                            source: ExtractionSource::TreeSitter,
                        });
                    }
                }
            }
        }
        _ => {}
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_nodes(child, path, source, nodes);
        }
    }
}
