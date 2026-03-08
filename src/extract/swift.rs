//! Swift tree-sitter extractor.
//!
//! Generic path: functions, classes/structs/enums, protocols, property fields, string literals.
//! Special case: module-level `let` bindings as Const nodes (text inspection).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::configs::SWIFT_CONFIG;
use super::generic::GenericExtractor;
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
        let mut result = GenericExtractor::new(&SWIFT_CONFIG).run(path, content)?;

        // Swift-specific: module-level `let` bindings as Const nodes.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_swift::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_let_consts(tree.root_node(), path, content.as_bytes(), &mut result.nodes);
        }

        Ok(result)
    }
}

/// Walk AST for module-level `let` bindings (Swift constant convention).
fn collect_let_consts(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    if node.kind() == "property_declaration" {
        let decl_text = node.utf8_text(source).unwrap_or("").to_string();
        // Only module-level (no class scope) `let` bindings
        if decl_text.trim_start().starts_with("let ") && !has_class_ancestor(node) {
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

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_let_consts(child, path, source, nodes);
        }
    }
}

/// Check if a node has a class/struct/enum ancestor (i.e. is NOT module-level).
fn has_class_ancestor(node: tree_sitter::Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "class_declaration" | "struct_declaration" | "enum_declaration" => return true,
            _ => {}
        }
        current = parent.parent();
    }
    false
}
