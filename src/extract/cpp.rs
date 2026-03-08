//! C++ tree-sitter extractor.
//!
//! Generic path: classes, structs, enums, fields, string literals.
//! Special cases:
//!   - `function_definition` with complex `function_declarator` name extraction
//!   - `namespace_definition` as Module nodes
//!   - `constexpr` / `static const` declarations as Const nodes

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::configs::CPP_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct CppExtractor;

impl CppExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for CppExtractor {
    fn extensions(&self) -> &[&str] {
        &["cpp", "cc", "cxx", "c", "h", "hpp", "hxx"]
    }

    fn name(&self) -> &str {
        "cpp-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut result = GenericExtractor::new(&CPP_CONFIG).run(path, content)?;

        // C++-specific: functions (complex declarator), namespaces, and const declarations.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_cpp::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_cpp_specials(tree.root_node(), path, content.as_bytes(), None, &mut result.nodes);
        }

        Ok(result)
    }
}

/// Extract the simple identifier name from a C++ declarator node.
/// Walks into qualified identifiers and function declarators to find the leaf identifier.
fn extract_cpp_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" | "destructor_name" => {
            Some(node.utf8_text(source).unwrap_or("unknown").to_string())
        }
        "qualified_identifier" => {
            node.child_by_field_name("name")
                .and_then(|n| extract_cpp_name(n, source))
        }
        "function_declarator" => {
            node.child_by_field_name("declarator")
                .and_then(|n| extract_cpp_name(n, source))
        }
        "pointer_declarator" | "reference_declarator" => {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if let Some(name) = extract_cpp_name(child, source) {
                        return Some(name);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Walk AST for C++-specific nodes: function_definition, namespace_definition, const declarations.
fn collect_cpp_specials(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    scope: Option<&str>,
    nodes: &mut Vec<Node>,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(declarator) = node.child_by_field_name("declarator") {
                if let Some(name) = extract_cpp_name(declarator, source) {
                    let qualified = match scope {
                        Some(s) => format!("{}::{}", s, name),
                        None => name.clone(),
                    };
                    let body = node.utf8_text(source).unwrap_or("").to_string();
                    let sig = body.lines().next().unwrap_or("").trim().to_string();

                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name: qualified,
                            kind: NodeKind::Function,
                        },
                        language: "c-cpp".to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        signature: sig,
                        body,
                        metadata: BTreeMap::new(),
                        source: ExtractionSource::TreeSitter,
                    });
                }
            }
        }
        "namespace_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("unknown").to_string();
                let qualified = match scope {
                    Some(s) => format!("{}::{}", s, name),
                    None => name.clone(),
                };

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: qualified.clone(),
                        kind: NodeKind::Module,
                    },
                    language: "c-cpp".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: format!("namespace {}", name),
                    body: String::new(),
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });

                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        collect_cpp_specials(child, path, source, Some(&qualified), nodes);
                    }
                }
                return;
            }
        }
        "declaration" => {
            let decl_text = node.utf8_text(source).unwrap_or("").to_string();
            let is_const = decl_text.contains("constexpr")
                || (decl_text.contains("static") && decl_text.contains("const "));
            if is_const {
                if let Some(declarator) = node.child_by_field_name("declarator") {
                    if let Some(name) = extract_cpp_name(declarator, source) {
                        let qualified = match scope {
                            Some(s) => format!("{}::{}", s, name),
                            None => name,
                        };
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
                                name: qualified,
                                kind: NodeKind::Const,
                            },
                            language: "c-cpp".to_string(),
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
        _ => {}
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_cpp_specials(child, path, source, scope, nodes);
        }
    }
}
