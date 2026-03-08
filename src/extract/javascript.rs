//! JavaScript tree-sitter extractor.
//!
//! Generic path: functions, generator functions, methods, classes, string literals.
//! Special cases: lexical_declaration const (text inspection), import_statement.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::configs::JAVASCRIPT_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct JavaScriptExtractor;

impl JavaScriptExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for JavaScriptExtractor {
    fn extensions(&self) -> &[&str] {
        &["js", "jsx", "mjs", "cjs"]
    }

    fn name(&self) -> &str {
        "javascript-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut result = GenericExtractor::new(&JAVASCRIPT_CONFIG).run(path, content)?;

        // JavaScript-specific: lexical_declaration const, import_statement.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_javascript::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_js_specials(
                tree.root_node(),
                path,
                content.as_bytes(),
                &mut result.nodes,
                &mut result.edges,
            );
        }

        Ok(result)
    }
}

/// Walk AST for JavaScript-specific nodes not handled by the generic extractor:
/// `lexical_declaration` (const) and `import_statement`.
fn collect_js_specials(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    match kind_str {
        "lexical_declaration" => {
            // `const FOO = 42;` at module level
            let decl_text = node.utf8_text(source).unwrap_or("").trim().to_string();
            if decl_text.starts_with("const ") {
                for i in 0..node.child_count() {
                    if let Some(decl) = node.child(i as u32) {
                        if decl.kind() == "variable_declarator" {
                            if let Some(name_node) = decl.child_by_field_name("name") {
                                let name_str = name_node.utf8_text(source).unwrap_or("unknown").trim().to_string();
                                if name_str.starts_with('{') || name_str.starts_with('[') {
                                    continue;
                                }
                                let value_str = decl
                                    .child_by_field_name("value")
                                    .and_then(|v| v.utf8_text(source).ok())
                                    .map(|s| s.trim().to_string());
                                let signature = decl_text.lines().next().unwrap_or("").trim().to_string();
                                let mut metadata = BTreeMap::new();
                                metadata.insert("name_col".to_string(), name_node.start_position().column.to_string());
                                if let Some(ref v) = value_str {
                                    let is_scalar = v.starts_with('"') || v.starts_with('\'')
                                        || v.starts_with('`')
                                        || v.parse::<f64>().is_ok()
                                        || v == "true" || v == "false";
                                    if is_scalar {
                                        let stripped = v.trim_matches('"').trim_matches('\'').trim_matches('`');
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
                                    language: "javascript".to_string(),
                                    line_start: node.start_position().row + 1,
                                    line_end: node.end_position().row + 1,
                                    signature,
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
        "import_statement" => {
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();
            let source_node = node.child_by_field_name("source");
            let target = source_node
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();

            let import_node = Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: text.clone(),
                    kind: NodeKind::Import,
                },
                language: "javascript".to_string(),
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
            return; // don't recurse into import statements
        }
        _ => {}
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_js_specials(child, path, source, nodes, edges);
        }
    }
}
