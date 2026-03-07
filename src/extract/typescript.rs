//! TypeScript tree-sitter extractor.
//!
//! Extracts functions, classes, interfaces, type aliases, and import statements
//! from TypeScript (and JavaScript) source files.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::{ExtractionResult, Extractor};

/// TypeScript tree-sitter extractor (handles .ts and .tsx files).
pub struct TypeScriptExtractor;

impl TypeScriptExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for TypeScriptExtractor {
    fn extensions(&self) -> &[&str] {
        &["ts", "tsx"]
    }

    fn name(&self) -> &str {
        "typescript-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        // Use tsx parser for both .ts and .tsx — tsx is a superset
        parser.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())?;
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
                let signature = extract_ts_signature(&body);

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name_str,
                        kind: NodeKind::Function,
                    },
                    language: "typescript".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature,
                    body,
                    metadata: BTreeMap::new(),
                });
            }
        }
        "class_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                let name_str = name.utf8_text(source).unwrap_or("unknown").to_string();
                let body = node.utf8_text(source).unwrap_or("").to_string();
                let signature = extract_ts_signature(&body);

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name_str,
                        kind: NodeKind::Struct,
                    },
                    language: "typescript".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature,
                    body,
                    metadata: BTreeMap::new(),
                });
            }
        }
        "interface_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                let name_str = name.utf8_text(source).unwrap_or("unknown").to_string();
                let body = node.utf8_text(source).unwrap_or("").to_string();
                let signature = extract_ts_signature(&body);

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name_str,
                        kind: NodeKind::Trait, // interface -> Trait in our model
                    },
                    language: "typescript".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature,
                    body,
                    metadata: BTreeMap::new(),
                });
            }
        }
        "type_alias_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                let name_str = name.utf8_text(source).unwrap_or("unknown").to_string();
                let body = node.utf8_text(source).unwrap_or("").to_string();

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name_str,
                        kind: NodeKind::Other("type_alias".to_string()),
                    },
                    language: "typescript".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: body.clone(),
                    body,
                    metadata: BTreeMap::new(),
                });
            }
        }
        "import_statement" => {
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();
            let target = parse_ts_import_source(&text);

            let import_node = Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: text.clone(),
                    kind: NodeKind::Import,
                },
                language: "typescript".to_string(),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                signature: text,
                body: String::new(),
                metadata: BTreeMap::new(),
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
        }
        _ => {}
    }

    // Recurse into children
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            collect_nodes(child, path, source, nodes, edges);
        }
    }
}

/// Extract signature from TypeScript code (text before first `{`).
fn extract_ts_signature(body: &str) -> String {
    if let Some(brace_pos) = body.find('{') {
        let sig = body[..brace_pos].trim();
        if !sig.is_empty() {
            return sig.to_string();
        }
    }
    body.lines().next().unwrap_or("").trim().to_string()
}

/// Parse the module source from a TypeScript import statement.
/// e.g., `import { Foo } from './bar';` -> `./bar`
fn parse_ts_import_source(text: &str) -> String {
    // Look for the 'from' clause
    if let Some(from_idx) = text.find(" from ") {
        let after = &text[from_idx + 6..];
        // Remove quotes and semicolons
        after
            .trim()
            .trim_matches(|c| c == '\'' || c == '"' || c == ';')
            .to_string()
    } else if text.starts_with("import ") {
        // Direct import: `import './side-effect';`
        text.strip_prefix("import ")
            .unwrap_or("")
            .trim()
            .trim_matches(|c| c == '\'' || c == '"' || c == ';')
            .to_string()
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_ts_functions_and_classes() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
function greet(name: string): string {
    return `Hello, ${name}`;
}

class UserService {
    private users: User[] = [];

    getUser(id: string): User | undefined {
        return this.users.find(u => u.id === id);
    }
}

interface User {
    id: string;
    name: string;
}

type UserId = string;
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

        let names: Vec<&str> = result.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"greet"), "Should find function greet");
        assert!(
            names.contains(&"UserService"),
            "Should find class UserService"
        );
        assert!(names.contains(&"User"), "Should find interface User");
        assert!(names.contains(&"UserId"), "Should find type alias UserId");
    }

    #[test]
    fn test_extract_ts_imports() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
import { Router } from 'express';
import path from 'path';
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

        let imports: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Import)
            .collect();
        assert_eq!(imports.len(), 2, "Should find 2 imports");

        let dep_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert_eq!(dep_edges.len(), 2, "Should produce 2 DependsOn edges");
    }

    #[test]
    fn test_ts_node_language() {
        let extractor = TypeScriptExtractor::new();
        let code = "function hello() {}\n";
        let result = extractor.extract(Path::new("app.ts"), code).unwrap();
        assert_eq!(result.nodes[0].language, "typescript");
    }

    #[test]
    fn test_ts_interface_is_trait_kind() {
        let extractor = TypeScriptExtractor::new();
        let code = "interface Foo {\n  bar: string;\n}\n";
        let result = extractor.extract(Path::new("app.ts"), code).unwrap();
        let iface = result
            .nodes
            .iter()
            .find(|n| n.id.name == "Foo")
            .expect("Should find Foo");
        assert_eq!(iface.id.kind, NodeKind::Trait);
    }

    #[test]
    fn test_tsx_extension_handled() {
        let extractor = TypeScriptExtractor::new();
        assert!(extractor.extensions().contains(&"tsx"));
    }
}
