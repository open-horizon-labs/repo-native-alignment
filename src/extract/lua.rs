//! Lua tree-sitter extractor.
//!
//! Extracts function declarations and local function definitions from `.lua` files.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

pub struct LuaExtractor;

impl LuaExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for LuaExtractor {
    fn extensions(&self) -> &[&str] {
        &["lua"]
    }

    fn name(&self) -> &str {
        "lua-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_lua::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let source = content.as_bytes();

        collect_nodes(tree.root_node(), path, source, &mut nodes);

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
            // function_declaration has a "name" field
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
                    language: "lua".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body,
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        "assignment_statement" => {
            // Lua ALL_CAPS global constants: `MAX_SIZE = 1024`
            // assignment_statement has a variable_list and expression_list
            if let Some(var_list) = node.child_by_field_name("variable_list")
                .or_else(|| (0..node.child_count()).find_map(|i| {
                    let c = node.child(i as u32)?;
                    if c.kind() == "variable_list" { Some(c) } else { None }
                }))
            {
                // Get first variable
                if let Some(name_node) = var_list.child(0 as u32) {
                    if name_node.kind() == "identifier" {
                        let name_str = name_node.utf8_text(source).unwrap_or("").trim().to_string();
                        if !name_str.is_empty()
                            && name_str.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                            && name_str.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false)
                        {
                            let body = node.utf8_text(source).unwrap_or("").to_string();
                            let sig = body.lines().next().unwrap_or("").trim().to_string();
                            // Get value from expression_list
                            let value_str = (0..node.child_count()).find_map(|i| {
                                let c = node.child(i as u32)?;
                                if c.kind() == "expression_list" {
                                    c.child(0 as u32)
                                        .and_then(|v| v.utf8_text(source).ok())
                                        .map(|s| s.trim().to_string())
                                } else { None }
                            });
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
                                    name: name_str,
                                    kind: NodeKind::Const,
                                },
                                language: "lua".to_string(),
                                line_start: node.start_position().row + 1,
                                line_end: node.end_position().row + 1,
                                signature: sig,
                                body,
                                metadata,
                                source: ExtractionSource::TreeSitter,
                            });
                        }
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
