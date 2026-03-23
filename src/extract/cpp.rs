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

impl Default for CppExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl CppExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for CppExtractor {
    fn extensions(&self) -> &[&str] {
        // .h is included here because header files are commonly shared between C and C++.
        // tree-sitter-cpp is a superset of tree-sitter-c and handles pure C headers correctly.
        // Only .c files go exclusively to the CExtractor (dedicated C grammar).
        &["cpp", "cc", "cxx", "h", "hpp", "hxx"]
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
                if let Some(child) = node.child(i as u32)
                    && let Some(name) = extract_cpp_name(child, source) {
                        return Some(name);
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
            if let Some(declarator) = node.child_by_field_name("declarator")
                && let Some(name) = extract_cpp_name(declarator, source) {
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
                        language: "cpp".to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        signature: sig,
                        body,
                        metadata: BTreeMap::new(),
                        source: ExtractionSource::TreeSitter,
                    });
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
                    language: "cpp".to_string(),
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
            if is_const
                && let Some(declarator) = node.child_by_field_name("declarator")
                    && let Some(name) = extract_cpp_name(declarator, source) {
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
                            language: "cpp".to_string(),
                            line_start: node.start_position().row + 1,
                            line_end: node.end_position().row + 1,
                            signature: sig,
                            body: decl_text,
                            metadata,
                            source: ExtractionSource::TreeSitter,
                        });
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_extract_cpp_object_like_macro() {
        let extractor = CppExtractor;
        let code = r#"
#define MAX_BUFFER_SIZE 1024
#define VERSION "1.0.0"
"#;
        let result = extractor.extract(Path::new("src/config.hpp"), code).unwrap();

        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        assert_eq!(macros.len(), 2, "Should find 2 object-like macros, got: {:?}",
            macros.iter().map(|n| &n.id.name).collect::<Vec<_>>());

        let names: Vec<&str> = macros.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"MAX_BUFFER_SIZE"), "Should find MAX_BUFFER_SIZE macro");
        assert!(names.contains(&"VERSION"), "Should find VERSION macro");
    }

    #[test]
    fn test_extract_cpp_function_like_macro() {
        let extractor = CppExtractor;
        let code = r#"
#define MIN(a, b) ((a) < (b) ? (a) : (b))
#define SQUARE(x) ((x) * (x))
"#;
        let result = extractor.extract(Path::new("src/util.hpp"), code).unwrap();

        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        assert_eq!(macros.len(), 2, "Should find 2 function-like macros, got: {:?}",
            macros.iter().map(|n| &n.id.name).collect::<Vec<_>>());

        let names: Vec<&str> = macros.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"MIN"), "Should find MIN macro");
        assert!(names.contains(&"SQUARE"), "Should find SQUARE macro");
    }

    #[test]
    fn test_cpp_macro_is_embeddable() {
        assert!(NodeKind::Macro.is_embeddable(), "Macro should be embeddable");
    }

    #[test]
    fn test_cpp_macros_mixed_with_functions() {
        let extractor = CppExtractor;
        let code = r#"
#define MAX_SIZE 100

int compute(int x) {
    return x * 2;
}

#define LOG(msg) printf("%s\n", msg)
"#;
        let result = extractor.extract(Path::new("src/util.cpp"), code).unwrap();

        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        let funcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();

        assert_eq!(macros.len(), 2, "Should find 2 macros");
        assert!(!funcs.is_empty(), "Should also find function(s)");
    }

    // -- Adversarial tests (seeded from dissent) --

    /// Dissent pre-mortem #2: include guard macros are noise but should still parse.
    #[test]
    fn test_cpp_include_guard_macros_extracted() {
        let extractor = CppExtractor;
        let code = r#"
#ifndef MY_HEADER_H
#define MY_HEADER_H

int useful_function(int x);

#endif
"#;
        let result = extractor.extract(Path::new("src/my_header.hpp"), code).unwrap();

        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        // Include guard #define should be extracted (it IS a preproc_def)
        assert!(
            macros.iter().any(|m| m.id.name == "MY_HEADER_H"),
            "Include guard macro should be extracted; got: {:?}",
            macros.iter().map(|n| &n.id.name).collect::<Vec<_>>()
        );
    }

    /// Adversarial: macro with same name as a function in same file.
    #[test]
    fn test_cpp_macro_and_function_same_name() {
        let extractor = CppExtractor;
        let code = r#"
#define compute(x) ((x) * 2)

int compute(int x) {
    return x * 2;
}
"#;
        let result = extractor.extract(Path::new("src/dual.cpp"), code).unwrap();

        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        let funcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();

        // Both should be extracted -- different NodeKinds means different NodeIds
        assert_eq!(macros.len(), 1, "Should find macro 'compute'");
        assert!(!funcs.is_empty(), "Should also find function 'compute'");
        assert_eq!(macros[0].id.name, "compute");
    }

    /// Adversarial: empty #define (no value).
    #[test]
    fn test_cpp_empty_define() {
        let extractor = CppExtractor;
        let code = "#define FEATURE_ENABLED\n";
        let result = extractor.extract(Path::new("src/config.hpp"), code).unwrap();

        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        assert_eq!(macros.len(), 1, "Empty #define should be extracted");
        assert_eq!(macros[0].id.name, "FEATURE_ENABLED");
    }

    /// Adversarial: multiline macro with backslash continuation.
    #[test]
    fn test_cpp_multiline_macro() {
        let extractor = CppExtractor;
        let code = r#"
#define MULTI_LINE_MACRO(x, y) \
    do { \
        printf("%d %d\n", x, y); \
    } while(0)
"#;
        let result = extractor.extract(Path::new("src/util.hpp"), code).unwrap();

        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        assert_eq!(macros.len(), 1, "Multiline macro should be extracted");
        assert_eq!(macros[0].id.name, "MULTI_LINE_MACRO");
    }
}
