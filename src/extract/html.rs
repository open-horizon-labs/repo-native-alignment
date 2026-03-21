//! HTML tree-sitter extractor.
//!
//! Extracts structural elements from HTML files:
//! - `<script>` blocks as embedded JS code nodes
//! - `id="..."` attribute values as anchor points
//! - `<template>` elements as template nodes
//!
//! This is a structural extractor — it does not parse embedded JavaScript
//! (that's handled by the JavaScript extractor on `.js` files). It surfaces
//! the HTML structure so agents can find where JS is embedded, what IDs exist,
//! and what templates are defined.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

pub struct HtmlExtractor;

impl HtmlExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for HtmlExtractor {
    fn extensions(&self) -> &[&str] {
        &["html", "htm"]
    }

    fn name(&self) -> &str {
        "html-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_html::LANGUAGE.into())?;

        let tree = match parser.parse(content, None) {
            Some(t) => t,
            None => return Ok(ExtractionResult::default()),
        };

        let mut nodes = Vec::new();
        collect_html_nodes(tree.root_node(), path, content.as_bytes(), &mut nodes);

        Ok(ExtractionResult { nodes, edges: vec![] })
    }
}

/// Walk the HTML AST to collect script blocks, template elements, and ID anchors.
fn collect_html_nodes(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    match node.kind() {
        "script_element" => {
            // Extract <script> blocks as embedded JS nodes.
            // Find the raw_text child which contains the actual JS content.
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if child.kind() == "raw_text" {
                        let body = child.utf8_text(source).unwrap_or("").to_string();
                        if !body.trim().is_empty() {
                            nodes.push(Node {
                                id: NodeId {
                                    root: String::new(),
                                    file: path.to_path_buf(),
                                    name: format!(
                                        "script@{}",
                                        node.start_position().row + 1
                                    ),
                                    kind: NodeKind::Other("html_script".to_string()),
                                },
                                language: "html".to_string(),
                                line_start: node.start_position().row + 1,
                                line_end: node.end_position().row + 1,
                                signature: format!(
                                    "<script> at line {}",
                                    node.start_position().row + 1
                                ),
                                body,
                                metadata: BTreeMap::new(),
                                source: ExtractionSource::TreeSitter,
                            });
                        }
                    }
                }
            }
        }
        "element" => {
            // Check if this is a <template> element or has an id attribute.
            let mut tag_name = None;
            let mut id_value = None;

            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if child.kind() == "start_tag" {
                        // Get tag name
                        if let Some(tag_node) = child.child_by_field_name("name") {
                            tag_name = tag_node.utf8_text(source).ok().map(|s| s.to_string());
                        } else {
                            // Try first child of start_tag
                            for j in 0..child.child_count() {
                                if let Some(tag_child) = child.child(j as u32) {
                                    if tag_child.kind() == "tag_name" {
                                        tag_name = tag_child.utf8_text(source).ok().map(|s| s.to_string());
                                        break;
                                    }
                                }
                            }
                        }

                        // Look for id attribute
                        for j in 0..child.child_count() {
                            if let Some(attr) = child.child(j as u32) {
                                if attr.kind() == "attribute" {
                                    let attr_text = attr.utf8_text(source).unwrap_or("");
                                    if attr_text.starts_with("id=") || attr_text.starts_with("id =") {
                                        // Extract the value
                                        for k in 0..attr.child_count() {
                                            if let Some(val_node) = attr.child(k as u32) {
                                                if val_node.kind() == "attribute_value" || val_node.kind() == "quoted_attribute_value" {
                                                    let raw = val_node.utf8_text(source).unwrap_or("");
                                                    id_value = Some(raw.trim_matches('"').trim_matches('\'').to_string());
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Emit <template> as a template node
            if tag_name.as_deref() == Some("template") {
                let body = node.utf8_text(source).unwrap_or("").to_string();
                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: format!("template@{}", node.start_position().row + 1),
                        kind: NodeKind::Other("html_template".to_string()),
                    },
                    language: "html".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: format!("<template> at line {}", node.start_position().row + 1),
                    body,
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });
            }

            // Emit id="..." as anchor node
            if let Some(id) = id_value {
                if !id.is_empty() {
                    let mut metadata = BTreeMap::new();
                    if let Some(ref tag) = tag_name {
                        metadata.insert("tag".to_string(), tag.clone());
                    }
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name: id.clone(),
                            kind: NodeKind::Other("html_id".to_string()),
                        },
                        language: "html".to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.start_position().row + 1,
                        signature: format!(
                            "#{} ({})",
                            id,
                            tag_name.as_deref().unwrap_or("element")
                        ),
                        body: String::new(),
                        metadata,
                        source: ExtractionSource::TreeSitter,
                    });
                }
            }
        }
        _ => {}
    }

    // Recurse into children
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_html_nodes(child, path, source, nodes);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_html_extract_script_blocks() {
        let extractor = HtmlExtractor::new();
        let code = r#"<!DOCTYPE html>
<html>
<head>
<script>
function greet() {
    console.log("hello");
}
</script>
</head>
<body></body>
</html>"#;
        let result = extractor.extract(Path::new("index.html"), code).unwrap();
        let scripts: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("html_script".to_string()))
            .collect();
        assert!(!scripts.is_empty(), "Should extract script blocks");
        assert!(scripts[0].body.contains("greet"), "Script body should contain JS");
    }

    #[test]
    fn test_html_extract_ids() {
        let extractor = HtmlExtractor::new();
        let code = r#"<!DOCTYPE html>
<html>
<body>
  <div id="main-content">Hello</div>
  <button id="submit-btn">Submit</button>
</body>
</html>"#;
        let result = extractor.extract(Path::new("page.html"), code).unwrap();
        let ids: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("html_id".to_string()))
            .collect();
        assert!(!ids.is_empty(), "Should extract HTML id attributes");
    }

    #[test]
    fn test_html_extractor_extensions() {
        let extractor = HtmlExtractor::new();
        assert!(extractor.extensions().contains(&"html"));
        assert!(extractor.extensions().contains(&"htm"));
        assert_eq!(extractor.name(), "html-tree-sitter");
    }

    #[test]
    fn test_html_empty_file() {
        let extractor = HtmlExtractor::new();
        let result = extractor.extract(Path::new("empty.html"), "").unwrap();
        assert!(result.nodes.is_empty());
    }
}
