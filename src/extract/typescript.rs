//! TypeScript tree-sitter extractor.
//!
//! Generic path: functions, methods, classes, interfaces, enums, fields, string literals.
//! Special cases: lexical_declaration const (text inspection), import_statement,
//! type_alias_declaration.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::configs::TYPESCRIPT_CONFIG;
use super::generic::GenericExtractor;
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
        let mut result = GenericExtractor::new(&TYPESCRIPT_CONFIG).run(path, content)?;

        // TypeScript-specific: lexical_declaration const, import_statement, type_alias_declaration.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_ts_specials(
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

/// Walk AST for TypeScript-specific nodes not handled by the generic extractor:
/// `lexical_declaration` (const), `import_statement`, `type_alias_declaration`.
fn collect_ts_specials(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    match kind_str {
        "lexical_declaration" => {
            // `const FOO = 42;` or `const BAR = "hello";`
            let decl_text = node.utf8_text(source).unwrap_or("").trim().to_string();
            if decl_text.starts_with("const ") {
                // Find variable_declarator children
                for i in 0..node.child_count() {
                    if let Some(decl) = node.child(i as u32) {
                        if decl.kind() == "variable_declarator" {
                            if let Some(name_node) = decl.child_by_field_name("name") {
                                let name_str = name_node.utf8_text(source).unwrap_or("unknown").trim().to_string();
                                // Skip destructuring patterns
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
                                    // Only store simple scalar literals
                                    let stripped = v.trim_matches('"').trim_matches('\'');
                                    let is_scalar = v.starts_with('"') || v.starts_with('\'')
                                        || v.parse::<f64>().is_ok()
                                        || v == "true" || v == "false";
                                    if is_scalar {
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
                                    language: "typescript".to_string(),
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
        }
        "type_alias_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                let name_str = name.utf8_text(source).unwrap_or("unknown").to_string();
                let body = node.utf8_text(source).unwrap_or("").to_string();
                let mut metadata = BTreeMap::new();
                metadata.insert("name_col".to_string(), name.start_position().column.to_string());

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
                    metadata,
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        _ => {}
    }

    // Recurse into children
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_ts_specials(child, path, source, nodes, edges);
        }
    }
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
    use crate::graph::EdgeKind;

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
