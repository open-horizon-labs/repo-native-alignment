//! HCL (HashiCorp Configuration Language) tree-sitter extractor.
//!
//! Extracts Terraform/OpenTofu resources, data sources, variables, modules,
//! outputs, and providers from `.tf` / `.hcl` files.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

pub struct HclExtractor;

impl HclExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for HclExtractor {
    fn extensions(&self) -> &[&str] {
        &["tf", "hcl", "tfvars"]
    }

    fn name(&self) -> &str {
        "hcl-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_hcl::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let source = content.as_bytes();

        collect_nodes(tree.root_node(), path, source, &mut nodes);

        Ok(ExtractionResult { nodes, edges: Vec::new() })
    }
}

/// Collect HCL block nodes.
///
/// HCL blocks don't use named fields — children are positional:
///   identifier (block_type) [string_lit|identifier]* (labels) block_start body block_end
fn collect_nodes(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    if node.kind() == "block" {
        let children: Vec<_> = (0..node.child_count())
            .filter_map(|i| node.child(i as u32))
            .collect();

        // First child should be the block type identifier
        let Some(type_node) = children.first() else {
            recurse(node, path, source, nodes);
            return;
        };
        if type_node.kind() != "identifier" {
            recurse(node, path, source, nodes);
            return;
        }
        let block_type = type_node.utf8_text(source).unwrap_or("").to_string();

        // Collect labels (string_lit or identifier children before block_start/body)
        let labels: Vec<String> = children
            .iter()
            .skip(1)
            .take_while(|c| c.kind() == "string_lit" || c.kind() == "identifier")
            .map(|c| {
                let text = c.utf8_text(source).unwrap_or("");
                // Strip surrounding quotes from string literals
                text.trim_matches('"').to_string()
            })
            .collect();

        let (kind, name) = block_name(&block_type, &labels);

        let mut metadata = BTreeMap::new();
        metadata.insert("block_type".to_string(), block_type.clone());
        for (i, label) in labels.iter().enumerate() {
            metadata.insert(format!("label_{}", i), label.clone());
        }

        nodes.push(Node {
            id: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name,
                kind,
            },
            language: "hcl".to_string(),
            line_start: node.start_position().row + 1,
            line_end: node.end_position().row + 1,
            signature: format!("{} {}", block_type, labels.join(" ")),
            body: String::new(),
            metadata,
            source: ExtractionSource::TreeSitter,
        });

        // For `variable` blocks, emit a Const node only when a default value is present
        if block_type == "variable" {
            if let Some(var_name) = labels.first() {
                // Try to find `default = <value>` in the block body
                let body_text = node.utf8_text(source).unwrap_or("").to_string();
                let default_val = extract_hcl_attr(&body_text, "default");
                if let Some(ref v) = default_val {
                    let mut const_metadata = BTreeMap::new();
                    const_metadata.insert("value".to_string(), v.clone());
                    const_metadata.insert("synthetic".to_string(), "false".to_string());
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name: var_name.clone(),
                            kind: NodeKind::Const,
                        },
                        language: "hcl".to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        signature: format!("variable \"{}\"", var_name),
                        body: body_text,
                        metadata: const_metadata,
                        source: ExtractionSource::TreeSitter,
                    });
                }
            }
        }

        // Don't recurse into block body for nested blocks (keeps graph flat)
        return;
    }

    recurse(node, path, source, nodes);
}

fn recurse(node: tree_sitter::Node, path: &Path, source: &[u8], nodes: &mut Vec<Node>) {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_nodes(child, path, source, nodes);
        }
    }
}

/// Extract a simple attribute value from HCL block body text.
/// e.g., `default = 5` returns `Some("5")`.
fn extract_hcl_attr(body: &str, attr: &str) -> Option<String> {
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(attr) {
            let rest = rest.trim();
            if let Some(val) = rest.strip_prefix('=') {
                let v = val.trim().trim_matches('"').trim_matches('\'').to_string();
                if !v.is_empty() && !v.starts_with('{') && !v.starts_with('[') {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// Map HCL block type + labels to a NodeKind and canonical name.
fn block_name(block_type: &str, labels: &[String]) -> (NodeKind, String) {
    match block_type {
        "resource" => {
            // resource "aws_s3_bucket" "my_bucket" → name: aws_s3_bucket.my_bucket
            let name = match (labels.first(), labels.get(1)) {
                (Some(a), Some(b)) => format!("{}.{}", a, b),
                (Some(a), None) => a.clone(),
                _ => "unknown".to_string(),
            };
            (NodeKind::Other("tf_resource".to_string()), name)
        }
        "data" => {
            let name = match (labels.first(), labels.get(1)) {
                (Some(a), Some(b)) => format!("data.{}.{}", a, b),
                (Some(a), None) => format!("data.{}", a),
                _ => "data".to_string(),
            };
            (NodeKind::Other("tf_data".to_string()), name)
        }
        "variable" => {
            let name = labels.first().map(|s| format!("var.{}", s)).unwrap_or_else(|| "var.unknown".to_string());
            (NodeKind::Other("tf_variable".to_string()), name)
        }
        "output" => {
            let name = labels.first().cloned().unwrap_or_else(|| "output".to_string());
            (NodeKind::Other("tf_output".to_string()), name)
        }
        "module" => {
            let name = labels.first().map(|s| format!("module.{}", s)).unwrap_or_else(|| "module.unknown".to_string());
            (NodeKind::Other("tf_module".to_string()), name)
        }
        "provider" => {
            let name = labels.first().cloned().unwrap_or_else(|| "provider".to_string());
            (NodeKind::Other("tf_provider".to_string()), name)
        }
        "terraform" => (NodeKind::Other("tf_config".to_string()), "terraform".to_string()),
        "locals" => (NodeKind::Other("tf_locals".to_string()), "locals".to_string()),
        other => {
            let name = if labels.is_empty() {
                other.to_string()
            } else {
                format!("{}.{}", other, labels.join("."))
            };
            (NodeKind::Other(format!("hcl_{}", other)), name)
        }
    }
}
