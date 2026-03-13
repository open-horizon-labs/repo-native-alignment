//! Java tree-sitter extractor.
//!
//! Generic path: classes, records, interfaces, enums, methods, constructors, fields, string literals.
//! Special cases: static final field -> Const upgrade (text inspection), import_declaration.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::configs::JAVA_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct JavaExtractor;

impl JavaExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for JavaExtractor {
    fn extensions(&self) -> &[&str] {
        &["java"]
    }

    fn name(&self) -> &str {
        "java-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut result = GenericExtractor::new(&JAVA_CONFIG).run(path, content)?;

        // Java-specific: upgrade static final fields to Const, add imports.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_java::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_java_specials(
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

/// Walk AST for Java-specific nodes not handled by the generic extractor:
/// `field_declaration` with `static final` -> Const upgrade, `import_declaration`.
fn collect_java_specials(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    match kind_str {
        "field_declaration" => {
            // static final fields are Java constants: `public static final int MAX = 5;`
            let decl_text = node.utf8_text(source).unwrap_or("").to_string();
            let has_static_final = decl_text.contains("static") && decl_text.contains("final");
            if has_static_final {
                // Remove the generic-emitted Field node for this line and replace with Const.
                let line = node.start_position().row + 1;
                nodes.retain(|n| !(n.id.kind == NodeKind::Field && n.line_start == line));

                // Find the variable_declarator child for name/value
                for i in 0..node.child_count() {
                    if let Some(decl) = node.child(i as u32) {
                        if decl.kind() == "variable_declarator" {
                            if let Some(name_node) = decl.child_by_field_name("name") {
                                let name = name_node.utf8_text(source).unwrap_or("unknown").trim().to_string();
                                let sig = decl_text.lines().next().unwrap_or("").trim().to_string();
                                let value_str = decl
                                    .child_by_field_name("value")
                                    .and_then(|v| v.utf8_text(source).ok())
                                    .map(|s| s.trim().to_string());
                                let mut metadata = BTreeMap::new();
                                metadata.insert("name_col".to_string(), name_node.start_position().column.to_string());
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
                                    language: "java".to_string(),
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
        "import_declaration" => {
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();
            // "import com.example.Foo;" -> "com.example.Foo"
            let target = text
                .strip_prefix("import ")
                .unwrap_or("")
                .trim_end_matches(';')
                .trim()
                .to_string();

            let import_node = Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: text.clone(),
                    kind: NodeKind::Import,
                },
                language: "java".to_string(),
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
            collect_java_specials(child, path, source, nodes, edges);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_java_interface_methods() {
        let extractor = JavaExtractor::new();
        let code = r#"
public interface Service {
    void serve(int port);
    String getName();
}
"#;
        let result = extractor.extract(Path::new("Service.java"), code).unwrap();

        // Interface itself should be found as Trait
        let service = result.nodes.iter().find(|n| n.id.name == "Service" && n.id.kind == NodeKind::Trait);
        assert!(service.is_some(), "Should find interface Service");

        // Interface methods should be indexed as Function nodes
        let serve = result.nodes.iter().find(|n| n.id.name == "serve" && n.id.kind == NodeKind::Function);
        assert!(serve.is_some(), "Should find interface method serve");

        let get_name = result.nodes.iter().find(|n| n.id.name == "getName" && n.id.kind == NodeKind::Function);
        assert!(get_name.is_some(), "Should find interface method getName");

        // Methods should have parent_scope pointing to the interface
        assert_eq!(
            serve.unwrap().metadata.get("parent_scope"),
            Some(&"Service".to_string()),
            "serve should have parent_scope = Service"
        );
        assert_eq!(
            get_name.unwrap().metadata.get("parent_scope"),
            Some(&"Service".to_string()),
            "getName should have parent_scope = Service"
        );
    }

    #[test]
    fn test_java_is_static() {
        let extractor = JavaExtractor::new();
        let code = r#"
public class MyService {
    public static MyService create() {
        return new MyService();
    }

    public void serve() {
        System.out.println("serving");
    }

    public static int count() {
        return 0;
    }
}
"#;
        let result = extractor.extract(Path::new("MyService.java"), code).unwrap();

        let create = result.nodes.iter().find(|n| n.id.name == "create" && n.id.kind == NodeKind::Function).unwrap();
        assert_eq!(create.metadata.get("is_static").map(|s| s.as_str()), Some("true"), "static create() should be static");

        let serve = result.nodes.iter().find(|n| n.id.name == "serve" && n.id.kind == NodeKind::Function).unwrap();
        assert_eq!(serve.metadata.get("is_static").map(|s| s.as_str()), Some("false"), "serve() should be instance");

        let count = result.nodes.iter().find(|n| n.id.name == "count" && n.id.kind == NodeKind::Function).unwrap();
        assert_eq!(count.metadata.get("is_static").map(|s| s.as_str()), Some("true"), "static count() should be static");
    }

    #[test]
    fn test_java_top_level_no_is_static() {
        // Java doesn't have top-level functions, but methods in classes should have is_static
        // while constructors inside classes should also have is_static
        let extractor = JavaExtractor::new();
        let code = r#"
public class Foo {
    public Foo() {}
}
"#;
        let result = extractor.extract(Path::new("Foo.java"), code).unwrap();

        let ctor = result.nodes.iter().find(|n| n.id.name == "Foo" && n.id.kind == NodeKind::Function).unwrap();
        assert_eq!(ctor.metadata.get("is_static").map(|s| s.as_str()), Some("false"), "Constructor should be instance (no static keyword)");
    }
}
