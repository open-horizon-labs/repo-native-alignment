//! Bash/Shell tree-sitter extractor.
//!
//! Extracts function definitions and notable variable assignments from shell scripts.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::string_literals::harvest_string_literals;
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
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_bash::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let source = content.as_bytes();

        collect_nodes(tree.root_node(), path, source, &mut nodes);

        // Harvest string literals as synthetic Const nodes.
        // Collect into a temporary vec so we can filter out interpolated strings.
        let mut string_nodes = Vec::new();
        harvest_string_literals(
            tree.root_node(),
            path,
            source,
            "bash",
            "string",
            None,
            &mut string_nodes,
        );
        // Filter: skip interpolated strings (stripped value contains `$`)
        for n in string_nodes {
            if n.id.name.contains('$') {
                continue;
            }
            nodes.push(n);
        }

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
        "function_definition" => {
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
                    language: "bash".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: sig,
                    body,
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        "variable_assignment" => {
            // Bash ALL_CAPS constants: `MAX_RETRIES=5` or `export API_URL="https://..."`
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
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    collect_nodes(child, path, source, nodes);
                }
            }
        }
        _ => {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    collect_nodes(child, path, source, nodes);
                }
            }
        }
    }
}
