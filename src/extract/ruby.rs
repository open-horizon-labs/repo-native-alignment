//! Ruby tree-sitter extractor.
//!
//! Generic path: methods, singleton methods, classes, modules, string literals.
//! Special case: `assignment` with `constant` LHS as Const nodes.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::configs::RUBY_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct RubyExtractor;

impl Default for RubyExtractor {
    fn default() -> Self {
        Self::new()
    }
}

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
        let mut result = GenericExtractor::new(&RUBY_CONFIG).run(path, content)?;

        // Ruby-specific: assignment with `constant` LHS as Const nodes.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_ruby::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_constant_assignments(
                tree.root_node(),
                path,
                content.as_bytes(),
                None,
                &mut result.nodes,
            );
        }

        Ok(result)
    }
}

/// Walk AST for assignments with `constant` LHS (Ruby constant convention).
fn collect_constant_assignments(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    scope: Option<&str>,
    nodes: &mut Vec<Node>,
) {
    match node.kind() {
        "assignment" => {
            if let Some(lhs) = node.child_by_field_name("left")
                && lhs.kind() == "constant"
            {
                let name_str = lhs.utf8_text(source).unwrap_or("").trim().to_string();
                if !name_str.is_empty()
                    && name_str
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_uppercase())
                        .unwrap_or(false)
                {
                    let body = node.utf8_text(source).unwrap_or("").to_string();
                    let sig = body.lines().next().unwrap_or("").trim().to_string();
                    let qualified = match scope {
                        Some(s) => format!("{}::{}", s, name_str),
                        None => name_str.clone(),
                    };
                    let value_str = node
                        .child_by_field_name("right")
                        .and_then(|v| v.utf8_text(source).ok())
                        .map(|s| s.trim().to_string());
                    let mut metadata = BTreeMap::new();
                    if let Some(ref v) = value_str {
                        let is_scalar = v.starts_with('"')
                            || v.starts_with('\'')
                            || v.starts_with(':')
                            || v.parse::<f64>().is_ok()
                            || v == "true"
                            || v == "false";
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
                        language: "ruby".to_string(),
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
        "class" | "singleton_class" | "module" => {
            // Track scope for qualified constant names
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        collect_constant_assignments(child, path, source, Some(&name), nodes);
                    }
                }
                return;
            }
        }
        _ => {}
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_constant_assignments(child, path, source, scope, nodes);
        }
    }
}
