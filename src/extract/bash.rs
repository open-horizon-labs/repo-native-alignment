//! Bash/Shell tree-sitter extractor.
//!
//! Generic path: function definitions, string literals.
//! Special case: ALL_CAPS `variable_assignment` as Const nodes.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::configs::BASH_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct BashExtractor;

impl BashExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for BashExtractor {
    fn extensions(&self) -> &[&str] {
        &["sh", "bash"]
    }

    fn name(&self) -> &str {
        "bash-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut result = GenericExtractor::new(&BASH_CONFIG).run(path, content)?;

        // Filter out interpolated strings from synthetic Const nodes.
        result.nodes.retain(|n| {
            !(n.id.kind == NodeKind::Const
                && n.metadata.get("synthetic").map(|s| s.as_str()) == Some("true")
                && n.id.name.contains('$'))
        });

        // Bash-specific: ALL_CAPS variable_assignment as Const nodes.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_bash::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_allcaps_consts(tree.root_node(), path, content.as_bytes(), &mut result.nodes);
        }

        Ok(result)
    }
}

/// Walk AST for ALL_CAPS variable assignments (Bash constant convention).
fn collect_allcaps_consts(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    if node.kind() == "variable_assignment" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name_str = name_node.utf8_text(source).unwrap_or("").trim().to_string();
            if !name_str.is_empty()
                && name_str.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && name_str.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false)
            {
                let body = node.utf8_text(source).unwrap_or("").to_string();
                let sig = body.lines().next().unwrap_or("").trim().to_string();
                let value_str = node.child_by_field_name("value")
                    .and_then(|v| v.utf8_text(source).ok())
                    .map(|s| s.trim().to_string());
                let mut metadata = BTreeMap::new();
                if let Some(ref v) = value_str {
                    let stripped = v.trim_matches('"').trim_matches('\'');
                    if !stripped.contains('\n') && stripped.len() < 200 {
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
                    language: "bash".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body,
                    metadata,
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        // Recurse into children of variable_assignment too
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                collect_allcaps_consts(child, path, source, nodes);
            }
        }
    } else {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                collect_allcaps_consts(child, path, source, nodes);
            }
        }
    }
}
