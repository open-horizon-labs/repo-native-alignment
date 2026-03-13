//! C# tree-sitter extractor.
//!
//! Generic path: classes, structs, records, interfaces, enums, methods, constructors, fields, string literals.
//! Special cases: const field -> Const upgrade (text inspection), using_directive.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::configs::CSHARP_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct CSharpExtractor;

impl CSharpExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for CSharpExtractor {
    fn extensions(&self) -> &[&str] {
        &["cs"]
    }

    fn name(&self) -> &str {
        "csharp-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut result = GenericExtractor::new(&CSHARP_CONFIG).run(path, content)?;

        // C#-specific: const field upgrade to Const, using_directive imports.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_c_sharp::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_csharp_specials(
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

/// Walk AST for C#-specific nodes not handled by the generic extractor:
/// `field_declaration` with `const` -> Const upgrade, `using_directive`.
fn collect_csharp_specials(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    match kind_str {
        "field_declaration" => {
            // C# constants: `public const int MAX_SIZE = 1024;`
            let decl_text = node.utf8_text(source).unwrap_or("").to_string();
            if decl_text.contains("const ") {
                // Remove the generic-emitted Field node for this line and replace with Const.
                let line = node.start_position().row + 1;
                nodes.retain(|n| !(n.id.kind == NodeKind::Field && n.line_start == line));

                // Walk variable_declarator children
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        // C# field_declaration -> variable_declaration -> variable_declarator
                        if child.kind() == "variable_declaration" {
                            for j in 0..child.child_count() {
                                if let Some(decl) = child.child(j as u32) {
                                    if decl.kind() == "variable_declarator" {
                                        if let Some(name_node) = decl.child_by_field_name("name") {
                                            let name = name_node.utf8_text(source).unwrap_or("unknown").trim().to_string();
                                            let sig = decl_text.lines().next().unwrap_or("").trim().to_string();
                                            // Value may be in equals_value_clause
                                            let value_str = decl.child_by_field_name("initializer")
                                                .and_then(|v| v.utf8_text(source).ok())
                                                .map(|s| s.trim_start_matches('=').trim().to_string());
                                            let mut metadata = BTreeMap::new();
                                            if let Some(ref v) = value_str {
                                                let is_scalar = v.starts_with('"') || v.starts_with('\'')
                                                    || v.parse::<f64>().is_ok()
                                                    || v == "true" || v == "false";
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
                                                language: "csharp".to_string(),
                                                line_start: node.start_position().row + 1,
                                                line_end: node.end_position().row + 1,
                                                signature: sig,
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
                }
            }
        }
        "using_directive" => {
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();
            let target = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();

            let import_node = Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: text.clone(),
                    kind: NodeKind::Import,
                },
                language: "csharp".to_string(),
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
            collect_csharp_specials(child, path, source, nodes, edges);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_csharp_is_static() {
        let extractor = CSharpExtractor::new();
        let code = r#"
public class MyService {
    public static MyService Create() {
        return new MyService();
    }

    public void Serve() {
        Console.WriteLine("serving");
    }

    public static int Count() {
        return 0;
    }
}
"#;
        let result = extractor.extract(Path::new("MyService.cs"), code).unwrap();

        let create = result.nodes.iter().find(|n| n.id.name == "Create" && n.id.kind == NodeKind::Function).unwrap();
        assert_eq!(create.metadata.get("is_static").map(|s| s.as_str()), Some("true"), "static Create() should be static");

        let serve = result.nodes.iter().find(|n| n.id.name == "Serve" && n.id.kind == NodeKind::Function).unwrap();
        assert_eq!(serve.metadata.get("is_static").map(|s| s.as_str()), Some("false"), "Serve() should be instance");

        let count = result.nodes.iter().find(|n| n.id.name == "Count" && n.id.kind == NodeKind::Function).unwrap();
        assert_eq!(count.metadata.get("is_static").map(|s| s.as_str()), Some("true"), "static Count() should be static");
    }

    #[test]
    fn test_csharp_constructor_is_instance() {
        let extractor = CSharpExtractor::new();
        let code = r#"
public class Foo {
    public Foo(int x) {
    }
}
"#;
        let result = extractor.extract(Path::new("Foo.cs"), code).unwrap();

        let ctor = result.nodes.iter().find(|n| n.id.name == "Foo" && n.id.kind == NodeKind::Function).unwrap();
        assert_eq!(ctor.metadata.get("is_static").map(|s| s.as_str()), Some("false"), "Constructor should be instance");
    }
}
