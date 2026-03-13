//! Go tree-sitter extractor.
//!
//! Generic path: functions, methods, string literals.
//! Special cases: const_declaration (multi-name pairing), type_declaration
//! (struct/interface disambiguation), import_declaration (grouped imports),
//! and method receiver metadata.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::configs::GO_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

/// Go tree-sitter extractor.
pub struct GoExtractor;

impl GoExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for GoExtractor {
    fn extensions(&self) -> &[&str] {
        &["go"]
    }

    fn name(&self) -> &str {
        "go-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        // Generic: functions + methods + string literals
        let mut result = GenericExtractor::new(&GO_CONFIG).run(path, content)?;

        // Go-specific: const_declaration, type_declaration, import_declaration,
        // and method receiver metadata enrichment.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_go::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_go_specials(
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

/// Walk the AST for Go-specific node kinds not handled by the generic extractor:
/// `const_declaration`, `type_declaration`, `import_declaration`, and
/// method receiver metadata enrichment on `method_declaration`.
fn collect_go_specials(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    match kind_str {
        "const_declaration" => {
            // Go const declarations: const X = 5 or const (X = 5; Y = "hello")
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if child.kind() == "const_spec" {
                        extract_go_const_spec(child, path, source, nodes);
                    }
                }
            }
            return; // Don't recurse into children we already handled
        }
        "var_declaration" => {
            // Go var declarations: var X = 5 or var (X int; Y string = "hello")
            // Single: var_declaration -> var_spec
            // Grouped: var_declaration -> var_spec_list -> var_spec*
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if child.kind() == "var_spec" {
                        extract_go_var_spec(child, path, source, nodes);
                    } else if child.kind() == "var_spec_list" {
                        for j in 0..child.child_count() {
                            if let Some(spec) = child.child(j as u32) {
                                if spec.kind() == "var_spec" {
                                    extract_go_var_spec(spec, path, source, nodes);
                                }
                            }
                        }
                    }
                }
            }
            return; // Don't recurse into children we already handled
        }
        "type_declaration" => {
            // type_declaration contains type_spec children
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if child.kind() == "type_spec" {
                        extract_type_spec(child, path, source, nodes, edges);
                    }
                }
            }
            return; // Don't recurse into children we already handled
        }
        "import_declaration" => {
            // Go imports can be single or grouped
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();

            // Find all import_spec children
            let mut found_specs = false;
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if child.kind() == "import_spec_list" {
                        for j in 0..child.child_count() {
                            if let Some(spec) = child.child(j as u32) {
                                if spec.kind() == "import_spec" {
                                    extract_import_spec(spec, path, source, nodes, edges);
                                    found_specs = true;
                                }
                            }
                        }
                    } else if child.kind() == "import_spec" {
                        extract_import_spec(child, path, source, nodes, edges);
                        found_specs = true;
                    }
                }
            }

            if !found_specs && !text.is_empty() {
                // Single import without import_spec children
                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: text.clone(),
                        kind: NodeKind::Import,
                    },
                    language: "go".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: text,
                    body: String::new(),
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                });
            }
            return; // Don't recurse
        }
        "method_declaration" => {
            // Enrich the generic-emitted method node with receiver metadata
            // and is_static = "false" (methods with receivers are instance methods).
            if let Some(name_node) = node.child_by_field_name("name") {
                let name_str = name_node.utf8_text(source).unwrap_or("unknown");
                if let Some(receiver) = node.child_by_field_name("receiver") {
                    let recv_text = receiver.utf8_text(source).unwrap_or("").to_string();
                    let line = node.start_position().row + 1;
                    // Find the matching node emitted by generic extractor
                    for n in nodes.iter_mut() {
                        if n.id.name == name_str
                            && n.id.kind == NodeKind::Function
                            && n.line_start == line
                        {
                            n.metadata.insert("receiver".to_string(), recv_text);
                            n.metadata.insert("is_static".to_string(), "false".to_string());
                            break;
                        }
                    }
                }
            }
        }
        "function_declaration" => {
            // Top-level Go functions are NOT methods, so they don't get is_static.
            // But functions declared at package level that are not methods are just
            // functions, not static methods. Skip -- no is_static for top-level.
        }
        _ => {}
    }

    // Recurse into children
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_go_specials(child, path, source, nodes, edges);
        }
    }
}

/// Extract a Go const_spec node (e.g., `MaxRetries = 5` or `A, B = 1, 2`).
///
/// In tree-sitter-go, `const_spec` may have multiple `name` children for
/// multi-name declarations like `const A, B = 1, 2`. We emit a Const node
/// for each name.
///
/// When the `value` child is an `expression_list`, we split it into individual
/// expressions and pair each with its corresponding name by index. If the value
/// count doesn't match the name count (e.g. `const A, B = iota`), we leave
/// `value` as None for all names rather than guessing.
fn extract_go_const_spec(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    let body = node.utf8_text(source).unwrap_or("").to_string();

    // Collect all name children (handles both single and multi-name specs).
    // In tree-sitter-go, `children_by_field_name("name")` may include comma
    // punctuation nodes (named=false) for multi-name specs -- filter those out.
    let mut cursor = node.walk();
    let names: Vec<String> = node
        .children_by_field_name("name", &mut cursor)
        .filter(|n| n.is_named())
        .map(|n| n.utf8_text(source).unwrap_or("unknown").trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // Build a per-name value list.
    // If the value child is an expression_list, extract each child expression
    // and pair by index. If counts don't match (e.g. `iota`), use None for all.
    let per_name_values: Vec<Option<String>> = if let Some(value_node) = node.child_by_field_name("value") {
        if value_node.kind() == "expression_list" {
            // Collect the non-punctuation children of the expression_list
            let exprs: Vec<String> = (0..value_node.child_count())
                .filter_map(|i| value_node.child(i as u32))
                .filter(|c| c.kind() != "," && c.is_named())
                .map(|c| c.utf8_text(source).unwrap_or("").trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            if exprs.len() == names.len() {
                exprs.into_iter().map(Some).collect()
            } else {
                // Count mismatch (e.g. iota): don't guess values
                vec![None; names.len()]
            }
        } else {
            // Single value node -- use it for every name (only one name expected)
            let v = value_node.utf8_text(source).unwrap_or("").trim().to_string();
            vec![Some(v); names.len()]
        }
    } else {
        vec![None; names.len()]
    };

    for (name_str, value_opt) in names.into_iter().zip(per_name_values.into_iter()) {
        let signature = format!("const {}", body.trim());
        let mut metadata = BTreeMap::new();
        if let Some(v) = value_opt {
            let is_scalar = v.starts_with('"') || v.starts_with('`')
                || v.parse::<f64>().is_ok()
                || v == "true" || v == "false";
            if is_scalar {
                let stripped = v.trim_matches('"').trim_matches('`');
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
            language: "go".to_string(),
            line_start: node.start_position().row + 1,
            line_end: node.end_position().row + 1,
            signature: signature.clone(),
            body: body.clone(),
            metadata,
            source: ExtractionSource::TreeSitter,
        });
    }
}

/// Extract a Go var_spec node (e.g., `X int = 5` or `X, Y = 1, 2`).
///
/// Mirrors `extract_go_const_spec` but emits `Const` nodes with
/// `metadata["storage"] = "var"` to distinguish package-level variables
/// from compile-time constants.
fn extract_go_var_spec(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    let body = node.utf8_text(source).unwrap_or("").to_string();

    // Collect all name children (handles both single and multi-name specs).
    let mut cursor = node.walk();
    let names: Vec<String> = node
        .children_by_field_name("name", &mut cursor)
        .filter(|n| n.is_named())
        .map(|n| n.utf8_text(source).unwrap_or("unknown").trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    // Build a per-name value list (same logic as const_spec).
    let per_name_values: Vec<Option<String>> = if let Some(value_node) = node.child_by_field_name("value") {
        if value_node.kind() == "expression_list" {
            let exprs: Vec<String> = (0..value_node.child_count())
                .filter_map(|i| value_node.child(i as u32))
                .filter(|c| c.kind() != "," && c.is_named())
                .map(|c| c.utf8_text(source).unwrap_or("").trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            if exprs.len() == names.len() {
                exprs.into_iter().map(Some).collect()
            } else {
                vec![None; names.len()]
            }
        } else {
            let v = value_node.utf8_text(source).unwrap_or("").trim().to_string();
            vec![Some(v); names.len()]
        }
    } else {
        vec![None; names.len()]
    };

    for (name_str, value_opt) in names.into_iter().zip(per_name_values.into_iter()) {
        let signature = format!("var {}", body.trim());
        let mut metadata = BTreeMap::new();
        metadata.insert("storage".to_string(), "var".to_string());
        if let Some(v) = value_opt {
            let is_scalar = v.starts_with('"') || v.starts_with('`')
                || v.parse::<f64>().is_ok()
                || v == "true" || v == "false";
            if is_scalar {
                let stripped = v.trim_matches('"').trim_matches('`');
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
            language: "go".to_string(),
            line_start: node.start_position().row + 1,
            line_end: node.end_position().row + 1,
            signature: signature.clone(),
            body: body.clone(),
            metadata,
            source: ExtractionSource::TreeSitter,
        });
    }
}

/// Extract a type_spec (e.g., `Foo struct { ... }` or `Bar interface { ... }`).
/// For interfaces, also extracts individual method_spec children as Function nodes
/// with `parent_scope` pointing to the interface name.
fn extract_type_spec(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    if let Some(name) = node.child_by_field_name("name") {
        let name_str = name.utf8_text(source).unwrap_or("unknown").to_string();
        let body = node.utf8_text(source).unwrap_or("").to_string();

        // Determine kind from the type body
        let type_node = node.child_by_field_name("type");
        let kind = type_node
            .map(|t| match t.kind() {
                "struct_type" => NodeKind::Struct,
                "interface_type" => NodeKind::Trait,
                _ => NodeKind::TypeAlias,
            })
            .unwrap_or(NodeKind::TypeAlias);

        let signature = format!("type {} ...", name_str);

        nodes.push(Node {
            id: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: name_str.clone(),
                kind: kind.clone(),
            },
            language: "go".to_string(),
            line_start: node.start_position().row + 1,
            line_end: node.end_position().row + 1,
            signature,
            body,
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        });

        // For interfaces, extract method_spec children as Function nodes.
        if kind == NodeKind::Trait {
            if let Some(type_body) = type_node {
                extract_interface_methods(type_body, path, source, &name_str, nodes, edges);
            }
        }
    }
}

/// Extract method_spec nodes from a Go interface_type body.
fn extract_interface_methods(
    interface_node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    interface_name: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    for i in 0..interface_node.child_count() {
        if let Some(child) = interface_node.child(i as u32) {
            if child.kind() == "method_elem" || child.kind() == "method_spec" {
                let method_name = child
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                    .unwrap_or("unknown")
                    .to_string();

                if method_name == "unknown" {
                    continue;
                }

                let method_body = child.utf8_text(source).unwrap_or("").to_string();
                let signature = method_body.lines().next().unwrap_or("").to_string();

                let mut metadata = BTreeMap::new();
                metadata.insert("parent_scope".to_string(), interface_name.to_string());

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: method_name.clone(),
                        kind: NodeKind::Function,
                    },
                    language: "go".to_string(),
                    line_start: child.start_position().row + 1,
                    line_end: child.end_position().row + 1,
                    signature,
                    body: method_body,
                    metadata,
                    source: ExtractionSource::TreeSitter,
                });

                // Emit structural edge from interface to method.
                edges.push(Edge {
                    from: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: interface_name.to_string(),
                        kind: NodeKind::Trait,
                    },
                    to: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: method_name,
                        kind: NodeKind::Function,
                    },
                    kind: EdgeKind::Defines,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }
        }
    }
}

/// Extract a single import spec and produce a node + edge.
fn extract_import_spec(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    // The import path is in a string child
    if let Some(path_node) = node.child_by_field_name("path") {
        let import_path = path_node
            .utf8_text(source)
            .unwrap_or("")
            .trim_matches('"')
            .to_string();

        if import_path.is_empty() {
            return;
        }

        let text = node.utf8_text(source).unwrap_or("").trim().to_string();

        let import_node = Node {
            id: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: text.clone(),
                kind: NodeKind::Import,
            },
            language: "go".to_string(),
            line_start: node.start_position().row + 1,
            line_end: node.end_position().row + 1,
            signature: text,
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        edges.push(Edge {
            from: import_node.id.clone(),
            to: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: import_path,
                kind: NodeKind::Module,
            },
            kind: EdgeKind::DependsOn,
            source: ExtractionSource::TreeSitter,
            confidence: Confidence::Detected,
        });

        nodes.push(import_node);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_go_functions_and_types() {
        let extractor = GoExtractor::new();
        let code = r#"package main

import "fmt"

func hello(name string) string {
	return fmt.Sprintf("Hello, %s", name)
}

type Config struct {
	Port int
	Host string
}

type Service interface {
	Serve() error
}

func (c *Config) Validate() error {
	return nil
}
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();

        let names: Vec<&str> = result.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"hello"), "Should find function hello");
        assert!(names.contains(&"Config"), "Should find struct Config");
        assert!(names.contains(&"Service"), "Should find interface Service");
        assert!(names.contains(&"Validate"), "Should find method Validate");
    }

    #[test]
    fn test_extract_go_imports() {
        let extractor = GoExtractor::new();
        let code = r#"package main

import (
	"fmt"
	"os"
	"net/http"
)

func main() {}
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();

        let imports: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Import)
            .collect();
        assert!(imports.len() >= 3, "Should find at least 3 imports, found {}", imports.len());

        let dep_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert!(dep_edges.len() >= 3, "Should produce at least 3 DependsOn edges");
    }

    #[test]
    fn test_go_struct_kind() {
        let extractor = GoExtractor::new();
        let code = "package main\n\ntype Foo struct {\n\tBar int\n}\n";
        let result = extractor.extract(Path::new("main.go"), code).unwrap();
        let foo = result
            .nodes
            .iter()
            .find(|n| n.id.name == "Foo")
            .expect("Should find Foo");
        assert_eq!(foo.id.kind, NodeKind::Struct);
    }

    #[test]
    fn test_go_interface_is_trait_kind() {
        let extractor = GoExtractor::new();
        let code = "package main\n\ntype Reader interface {\n\tRead(p []byte) (n int, err error)\n}\n";
        let result = extractor.extract(Path::new("main.go"), code).unwrap();
        let reader = result
            .nodes
            .iter()
            .find(|n| n.id.name == "Reader")
            .expect("Should find Reader");
        assert_eq!(reader.id.kind, NodeKind::Trait);
    }

    #[test]
    fn test_go_node_language() {
        let extractor = GoExtractor::new();
        let code = "package main\n\nfunc hello() {}\n";
        let result = extractor.extract(Path::new("main.go"), code).unwrap();
        let func = result
            .nodes
            .iter()
            .find(|n| n.id.kind == NodeKind::Function)
            .expect("Should find a function");
        assert_eq!(func.language, "go");
    }

    #[test]
    fn test_go_method_has_receiver() {
        let extractor = GoExtractor::new();
        let code = r#"package main

type Foo struct{}

func (f *Foo) Bar() {}
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();
        let method = result
            .nodes
            .iter()
            .find(|n| n.id.name == "Bar")
            .expect("Should find method Bar");
        assert!(method.metadata.contains_key("receiver"));
    }

    #[test]
    fn test_go_method_is_static() {
        let extractor = GoExtractor::new();
        let code = r#"package main

type Foo struct{}

func (f *Foo) Bar() {}

func hello() {}
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();

        // Method with receiver should be is_static = false
        let bar = result.nodes.iter().find(|n| n.id.name == "Bar").unwrap();
        assert_eq!(bar.metadata.get("is_static").map(|s| s.as_str()), Some("false"), "Method with receiver should not be static");

        // Top-level function should NOT have is_static
        let hello = result.nodes.iter().find(|n| n.id.name == "hello").unwrap();
        assert!(hello.metadata.get("is_static").is_none(), "Top-level Go function should NOT have is_static");
    }

    #[test]
    fn test_go_const_extraction() {
        let extractor = GoExtractor::new();
        let code = r#"package main

const MaxRetries = 5
const (
    StatusOK = 200
    StatusNotFound = 404
)
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();
        let consts: Vec<_> = result.nodes.iter().filter(|n| n.id.kind == NodeKind::Const).collect();
        assert!(!consts.is_empty(), "Should find const nodes");
        assert_eq!(consts[0].metadata.get("synthetic").map(|s| s.as_str()), Some("false"));
    }

    #[test]
    fn test_go_multi_name_const() {
        let extractor = GoExtractor::new();
        let code = r#"package main

const A, B = 1, 2
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();
        let consts: Vec<_> = result.nodes.iter().filter(|n| n.id.kind == NodeKind::Const).collect();
        let names: Vec<&str> = consts.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"A"), "Should find const A, got: {:?}", names);
        assert!(names.contains(&"B"), "Should find const B, got: {:?}", names);

        // Each name should have its own paired value
        let a = consts.iter().find(|n| n.id.name == "A").expect("Should find A");
        let b = consts.iter().find(|n| n.id.name == "B").expect("Should find B");
        assert_eq!(
            a.metadata.get("value").map(|s| s.as_str()),
            Some("1"),
            "A should have value 1"
        );
        assert_eq!(
            b.metadata.get("value").map(|s| s.as_str()),
            Some("2"),
            "B should have value 2"
        );
    }

    #[test]
    fn test_go_type_alias_kind() {
        let extractor = GoExtractor::new();
        let code = r#"package main

type Handler func(w http.ResponseWriter, r *http.Request)
type Duration int64
type Pair [2]string
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();

        let handler = result
            .nodes
            .iter()
            .find(|n| n.id.name == "Handler")
            .expect("Should find Handler");
        assert_eq!(handler.id.kind, NodeKind::TypeAlias, "Function type alias should be TypeAlias");

        let duration = result
            .nodes
            .iter()
            .find(|n| n.id.name == "Duration")
            .expect("Should find Duration");
        assert_eq!(duration.id.kind, NodeKind::TypeAlias, "Primitive type alias should be TypeAlias");

        let pair = result
            .nodes
            .iter()
            .find(|n| n.id.name == "Pair")
            .expect("Should find Pair");
        assert_eq!(pair.id.kind, NodeKind::TypeAlias, "Array type alias should be TypeAlias");
    }

    #[test]
    fn test_go_struct_and_interface_still_correct() {
        // Ensure struct and interface types are NOT affected by the TypeAlias change
        let extractor = GoExtractor::new();
        let code = r#"package main

type Config struct {
	Port int
}

type Service interface {
	Serve() error
}
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();

        let config = result.nodes.iter().find(|n| n.id.name == "Config").expect("Should find Config");
        assert_eq!(config.id.kind, NodeKind::Struct, "Struct type should still be Struct");

        let service = result.nodes.iter().find(|n| n.id.name == "Service").expect("Should find Service");
        assert_eq!(service.id.kind, NodeKind::Trait, "Interface type should still be Trait");
    }

    #[test]
    fn test_go_multi_name_const_iota_no_value() {
        let extractor = GoExtractor::new();
        let code = r#"package main

const (
    A = iota
    B
    C
)
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();
        let consts: Vec<_> = result.nodes.iter().filter(|n| n.id.kind == NodeKind::Const).collect();
        let names: Vec<&str> = consts.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"A"), "Should find const A");
        // iota is not a scalar, so value should not be set for A
        let a = consts.iter().find(|n| n.id.name == "A").expect("Should find A");
        assert!(
            a.metadata.get("value").is_none(),
            "A with iota should not have a scalar value, got: {:?}",
            a.metadata.get("value")
        );
    }

    #[test]
    fn test_go_var_declaration_single() {
        let extractor = GoExtractor::new();
        let code = r#"package main

var Debug = false
var Version string = "1.0.0"
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();

        let vars: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Const && n.metadata.get("storage").map(|s| s.as_str()) == Some("var"))
            .collect();
        let names: Vec<&str> = vars.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"Debug"), "Should find var Debug, got: {:?}", names);
        assert!(names.contains(&"Version"), "Should find var Version, got: {:?}", names);

        let debug = vars.iter().find(|n| n.id.name == "Debug").unwrap();
        assert_eq!(
            debug.metadata.get("value").map(|s| s.as_str()),
            Some("false"),
            "Debug should have value=false"
        );
        assert_eq!(
            debug.metadata.get("synthetic").map(|s| s.as_str()),
            Some("false"),
            "Debug should be non-synthetic"
        );

        let version = vars.iter().find(|n| n.id.name == "Version").unwrap();
        assert_eq!(
            version.metadata.get("value").map(|s| s.as_str()),
            Some("1.0.0"),
            "Version should have value=1.0.0"
        );
    }

    #[test]
    fn test_go_var_declaration_grouped() {
        let extractor = GoExtractor::new();
        let code = r#"package main

var (
    Logger  *log.Logger
    Verbose bool = true
)
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();

        let vars: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Const && n.metadata.get("storage").map(|s| s.as_str()) == Some("var"))
            .collect();
        let names: Vec<&str> = vars.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"Logger"), "Should find var Logger, got: {:?}", names);
        assert!(names.contains(&"Verbose"), "Should find var Verbose, got: {:?}", names);

        let verbose = vars.iter().find(|n| n.id.name == "Verbose").unwrap();
        assert_eq!(
            verbose.metadata.get("value").map(|s| s.as_str()),
            Some("true"),
            "Verbose should have value=true"
        );
    }

    #[test]
    fn test_go_var_multi_name() {
        let extractor = GoExtractor::new();
        let code = r#"package main

var X, Y = 1, 2
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();

        let vars: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Const && n.metadata.get("storage").map(|s| s.as_str()) == Some("var"))
            .collect();
        let names: Vec<&str> = vars.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"X"), "Should find var X, got: {:?}", names);
        assert!(names.contains(&"Y"), "Should find var Y, got: {:?}", names);

        let x = vars.iter().find(|n| n.id.name == "X").unwrap();
        assert_eq!(x.metadata.get("value").map(|s| s.as_str()), Some("1"));
        let y = vars.iter().find(|n| n.id.name == "Y").unwrap();
        assert_eq!(y.metadata.get("value").map(|s| s.as_str()), Some("2"));
    }

    #[test]
    fn test_go_var_distinguished_from_const() {
        let extractor = GoExtractor::new();
        let code = r#"package main

const MaxRetries = 5
var CurrentRetries = 0
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();

        let max = result.nodes.iter().find(|n| n.id.name == "MaxRetries").expect("Should find MaxRetries");
        assert!(
            max.metadata.get("storage").is_none(),
            "const should not have storage metadata"
        );

        let current = result.nodes.iter().find(|n| n.id.name == "CurrentRetries").expect("Should find CurrentRetries");
        assert_eq!(
            current.metadata.get("storage").map(|s| s.as_str()),
            Some("var"),
            "var should have storage=var"
        );
    }

    #[test]
    fn test_go_var_signature_contains_var_keyword() {
        let extractor = GoExtractor::new();
        let code = "package main\n\nvar Logger *log.Logger\n";
        let result = extractor.extract(Path::new("main.go"), code).unwrap();

        let logger = result
            .nodes
            .iter()
            .find(|n| n.id.name == "Logger")
            .expect("Should find Logger");
        assert!(
            logger.signature.contains("var"),
            "Var item signature should contain 'var', got: {}",
            logger.signature
        );
    }

    /// Adversarial: var with no initializer (type-only declaration)
    #[test]
    fn test_go_var_no_initializer() {
        let extractor = GoExtractor::new();
        let code = r#"package main

var Counter int
var Name string
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();
        let vars: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Const && n.metadata.get("storage").map(|s| s.as_str()) == Some("var"))
            .collect();
        let names: Vec<&str> = vars.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"Counter"), "Should find var Counter, got: {:?}", names);
        assert!(names.contains(&"Name"), "Should find var Name, got: {:?}", names);
        // No value should be extracted for type-only declarations
        let counter = vars.iter().find(|n| n.id.name == "Counter").unwrap();
        assert!(counter.metadata.get("value").is_none(), "Type-only var should have no value");
    }

    /// Adversarial: function-local var declarations are also captured
    /// (Go doesn't syntactically distinguish package-level from function-local `var`)
    #[test]
    fn test_go_function_local_var_also_captured() {
        let extractor = GoExtractor::new();
        let code = r#"package main

var GlobalVar = 42

func main() {
    var localVar = "hello"
    _ = localVar
}
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();
        let vars: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Const && n.metadata.get("storage").map(|s| s.as_str()) == Some("var"))
            .collect();
        let names: Vec<&str> = vars.iter().map(|n| n.id.name.as_str()).collect();
        // Both should be captured (known limitation: we don't filter by scope)
        assert!(names.contains(&"GlobalVar"), "Should find global var");
        assert!(names.contains(&"localVar"), "Function-local var also captured (known behavior)");
    }

    #[test]
    fn test_extract_go_interface_method_specs() {
        let extractor = GoExtractor::new();
        let code = r#"package main

type Reader interface {
	Read(p []byte) (n int, err error)
	Close() error
}
"#;
        let result = extractor.extract(Path::new("main.go"), code).unwrap();

        // Interface itself should be found as Trait
        let reader = result.nodes.iter().find(|n| n.id.name == "Reader" && n.id.kind == NodeKind::Trait);
        assert!(reader.is_some(), "Should find interface Reader");

        // Method specs should be indexed as Function nodes
        let read = result.nodes.iter().find(|n| n.id.name == "Read" && n.id.kind == NodeKind::Function);
        assert!(read.is_some(), "Should find interface method Read");

        let close = result.nodes.iter().find(|n| n.id.name == "Close" && n.id.kind == NodeKind::Function);
        assert!(close.is_some(), "Should find interface method Close");

        // Methods should have parent_scope pointing to the interface
        assert_eq!(
            read.unwrap().metadata.get("parent_scope"),
            Some(&"Reader".to_string()),
            "Read should have parent_scope = Reader"
        );
        assert_eq!(
            close.unwrap().metadata.get("parent_scope"),
            Some(&"Reader".to_string()),
            "Close should have parent_scope = Reader"
        );

        // Should produce Defines edges from interface to methods
        let defines_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Defines && e.from.name == "Reader")
            .collect();
        assert!(
            defines_edges.iter().any(|e| e.to.name == "Read"),
            "Should have Defines edge Reader -> Read"
        );
        assert!(
            defines_edges.iter().any(|e| e.to.name == "Close"),
            "Should have Defines edge Reader -> Close"
        );
    }
}
