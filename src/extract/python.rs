//! Python tree-sitter extractor.
//!
//! Generic path: functions, classes, imports, string literals.
//! Special case: ALL_CAPS module-level assignments as Const nodes.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::configs::PYTHON_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct PythonExtractor;

impl PythonExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for PythonExtractor {
    fn extensions(&self) -> &[&str] {
        &["py"]
    }

    fn name(&self) -> &str {
        "python-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut result = GenericExtractor::new(&PYTHON_CONFIG).run(path, content)?;

        // Python-specific: ALL_CAPS module-level assignments as Const nodes.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_allcaps_consts(tree.root_node(), path, content.as_bytes(), &mut result.nodes);
        }

        Ok(result)
    }
}

/// Walk AST for module-level ALL_CAPS assignments (Python constant convention).
fn collect_allcaps_consts(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    if node.kind() == "expression_statement" {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if child.kind() == "assignment" {
                    if let Some(lhs) = child.child_by_field_name("left") {
                        let name_str = lhs.utf8_text(source).unwrap_or("").trim().to_string();
                        if !name_str.is_empty()
                            && name_str.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false)
                            && name_str.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                        {
                            let value_str = child
                                .child_by_field_name("right")
                                .and_then(|v| v.utf8_text(source).ok())
                                .map(|s| s.trim().to_string());
                            let body = child.utf8_text(source).unwrap_or("").to_string();
                            let signature = body.lines().next().unwrap_or("").trim().to_string();
                            let mut metadata = BTreeMap::new();
                            if let Some(ref v) = value_str {
                                if !v.starts_with('[') && !v.starts_with('{') && !v.starts_with('(') {
                                    metadata.insert("value".to_string(), v.clone());
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
                                language: "python".to_string(),
                                line_start: child.start_position().row + 1,
                                line_end: child.end_position().row + 1,
                                signature,
                                body,
                                metadata,
                                source: ExtractionSource::TreeSitter,
                            });
                        }
                    }
                }
            }
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_allcaps_consts(child, path, source, nodes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::EdgeKind;

    #[test]
    fn test_extract_python_functions_and_classes() {
        let extractor = PythonExtractor::new();
        let code = r#"
def hello(name: str) -> str:
    return f"Hello, {name}"

class Config:
    def __init__(self, port: int):
        self.port = port

    def validate(self):
        pass
"#;
        let result = extractor.extract(Path::new("app.py"), code).unwrap();

        let names: Vec<&str> = result.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"hello"), "Should find function hello");
        assert!(names.contains(&"Config"), "Should find class Config");
        assert!(names.contains(&"__init__"), "Should find __init__ method");
        assert!(names.contains(&"validate"), "Should find validate method");
    }

    #[test]
    fn test_extract_python_imports() {
        let extractor = PythonExtractor::new();
        let code = r#"
import os
from pathlib import Path
from typing import Optional
"#;
        let result = extractor.extract(Path::new("app.py"), code).unwrap();

        let imports: Vec<_> = result.nodes.iter().filter(|n| n.id.kind == NodeKind::Import).collect();
        assert!(imports.len() >= 3, "Should find at least 3 imports, got {}", imports.len());

        let dep_edges: Vec<_> = result.edges.iter().filter(|e| e.kind == EdgeKind::DependsOn).collect();
        assert!(dep_edges.len() >= 3, "Should produce at least 3 DependsOn edges, got {}", dep_edges.len());
    }

    #[test]
    fn test_python_node_language() {
        let extractor = PythonExtractor::new();
        let code = "def hello():\n    pass\n";
        let result = extractor.extract(Path::new("app.py"), code).unwrap();
        assert_eq!(result.nodes[0].language, "python");
    }

    #[test]
    fn test_python_class_is_struct_kind() {
        let extractor = PythonExtractor::new();
        let code = "class Foo:\n    pass\n";
        let result = extractor.extract(Path::new("app.py"), code).unwrap();
        let class_node = result.nodes.iter().find(|n| n.id.name == "Foo").expect("Should find Foo");
        assert_eq!(class_node.id.kind, NodeKind::Struct);
    }

    #[test]
    fn test_python_allcaps_constants() {
        let extractor = PythonExtractor::new();
        let code = r#"
MAX_RETRIES = 5
API_URL = "https://api.example.com"
not_a_constant = 42
CamelCase = "also not a constant"
"#;
        let result = extractor.extract(Path::new("config.py"), code).unwrap();
        let consts: Vec<_> = result.nodes.iter().filter(|n| n.id.kind == NodeKind::Const).collect();
        let declared: Vec<_> = consts.iter().filter(|n| n.metadata.get("synthetic").map(|s| s.as_str()) == Some("false")).collect();
        assert_eq!(declared.len(), 2, "Should find 2 declared ALL_CAPS constants");
        let names: Vec<&str> = declared.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"MAX_RETRIES"), "Should find MAX_RETRIES");
        assert!(names.contains(&"API_URL"), "Should find API_URL");
        let max_retries = declared.iter().find(|n| n.id.name == "MAX_RETRIES").unwrap();
        assert_eq!(max_retries.metadata.get("value").map(|s| s.as_str()), Some("5"));
        assert_eq!(max_retries.metadata.get("synthetic").map(|s| s.as_str()), Some("false"));
    }
}
