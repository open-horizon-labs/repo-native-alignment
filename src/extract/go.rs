//! Go tree-sitter extractor.
//!
//! Extracts functions, methods, type declarations, and import statements
//! from Go source files.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

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
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_go::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let source = content.as_bytes();

        collect_nodes(tree.root_node(), path, source, &mut nodes, &mut edges);

        Ok(ExtractionResult { nodes, edges })
    }
}

fn collect_nodes(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    match kind_str {
        "function_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                let name_str = name.utf8_text(source).unwrap_or("unknown").to_string();
                let body = node.utf8_text(source).unwrap_or("").to_string();
                let signature = extract_go_signature(&body);

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name_str,
                        kind: NodeKind::Function,
                    },
                    language: "go".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature,
                    body,
                    metadata: BTreeMap::new(),
                });
            }
        }
        "method_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                let name_str = name.utf8_text(source).unwrap_or("unknown").to_string();
                let body = node.utf8_text(source).unwrap_or("").to_string();
                let signature = extract_go_signature(&body);

                // Extract receiver type for metadata
                let mut metadata = BTreeMap::new();
                if let Some(receiver) = node.child_by_field_name("receiver") {
                    let recv_text = receiver.utf8_text(source).unwrap_or("").to_string();
                    metadata.insert("receiver".to_string(), recv_text);
                }

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name_str,
                        kind: NodeKind::Function, // methods are functions in our model
                    },
                    language: "go".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature,
                    body,
                    metadata,
                });
            }
        }
        "type_declaration" => {
            // type_declaration contains type_spec children
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "type_spec" {
                        extract_type_spec(child, path, source, nodes);
                    }
                }
            }
        }
        "import_declaration" => {
            // Go imports can be single or grouped
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();

            // Find all import_spec children
            let mut found_specs = false;
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "import_spec_list" {
                        for j in 0..child.child_count() {
                            if let Some(spec) = child.child(j) {
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
                });
            }
        }
        _ => {}
    }

    // Recurse into children (but skip type_declaration and import_declaration
    // which we handle above)
    if kind_str != "type_declaration" && kind_str != "import_declaration" {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                collect_nodes(child, path, source, nodes, edges);
            }
        }
    }
}

/// Extract a type_spec (e.g., `Foo struct { ... }` or `Bar interface { ... }`).
fn extract_type_spec(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
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
                _ => NodeKind::Other("type".to_string()),
            })
            .unwrap_or(NodeKind::Other("type".to_string()));

        let signature = format!("type {} ...", name_str);

        nodes.push(Node {
            id: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: name_str,
                kind,
            },
            language: "go".to_string(),
            line_start: node.start_position().row + 1,
            line_end: node.end_position().row + 1,
            signature,
            body,
            metadata: BTreeMap::new(),
        });
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

/// Extract signature from Go code (text before first `{`).
fn extract_go_signature(body: &str) -> String {
    if let Some(brace_pos) = body.find('{') {
        let sig = body[..brace_pos].trim();
        if !sig.is_empty() {
            return sig.to_string();
        }
    }
    body.lines().next().unwrap_or("").trim().to_string()
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
}
