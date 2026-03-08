//! Zig tree-sitter extractor.
//!
//! Generic path: functions, structs, enums, string literals.
//! Special case: `const` variable declarations as Const nodes (text inspection).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::configs::ZIG_CONFIG;
use super::generic::GenericExtractor;
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
        let mut result = GenericExtractor::new(&ZIG_CONFIG).run(path, content)?;

        // Zig-specific: `const` variable declarations as Const nodes.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_zig::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_const_vars(tree.root_node(), path, content.as_bytes(), &mut result.nodes);
        }

        Ok(result)
    }
}

/// Walk AST for `const` variable declarations (Zig constant convention).
fn collect_const_vars(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    if node.kind() == "variable_declaration" {
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

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_const_vars(child, path, source, nodes);
        }
    }
}
