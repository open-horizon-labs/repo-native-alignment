//! Kotlin tree-sitter extractor.
//!
//! Generic path: functions, classes, objects, enum bodies, properties (fields), string literals.
//! Special cases: const val -> Const upgrade (text inspection), import handling.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

use super::configs::KOTLIN_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct KotlinExtractor;

impl Default for KotlinExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl KotlinExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for KotlinExtractor {
    fn extensions(&self) -> &[&str] {
        &["kt", "kts"]
    }

    fn name(&self) -> &str {
        "kotlin-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut result = GenericExtractor::new(&KOTLIN_CONFIG).run(path, content)?;

        // Kotlin-specific: const val upgrade to Const, import handling.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_kotlin_specials(
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

/// Walk AST for Kotlin-specific nodes not handled by the generic extractor:
/// `property_declaration` with `const` -> Const upgrade, `import`.
fn collect_kotlin_specials(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    match kind_str {
        "property_declaration" => {
            // Kotlin constants: `const val MAX_SIZE = 1024`
            let decl_text = node.utf8_text(source).unwrap_or("").to_string();
            if decl_text.contains("const ") {
                // Remove the generic-emitted Field node for this line and replace with Const.
                let line = node.start_position().row + 1;
                nodes.retain(|n| !(n.id.kind == NodeKind::Field && n.line_start == line));

                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(source)
                        .unwrap_or("unknown")
                        .trim()
                        .to_string();
                    let sig = decl_text.lines().next().unwrap_or("").trim().to_string();
                    // Value is after the `=` sign
                    let value_str = decl_text
                        .find('=')
                        .map(|pos| decl_text[pos + 1..].trim().to_string())
                        .filter(|s| !s.is_empty());
                    let mut metadata = BTreeMap::new();
                    if let Some(ref v) = value_str {
                        let is_scalar = v.starts_with('"')
                            || v.starts_with('\'')
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
                            name,
                            kind: NodeKind::Const,
                        },
                        language: "kotlin".to_string(),
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
        "import" => {
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();
            // "import foo.bar.Baz" -> "foo.bar.Baz"
            let target = text
                .strip_prefix("import ")
                .map(|s| s.trim().to_string())
                .unwrap_or_default();

            let import_node = Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: text.clone(),
                    kind: NodeKind::Import,
                },
                language: "kotlin".to_string(),
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
            return;
        }
        _ => {}
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_kotlin_specials(child, path, source, nodes, edges);
        }
    }
}
