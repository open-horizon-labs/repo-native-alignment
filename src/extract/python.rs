//! Python tree-sitter extractor.
//!
//! Extracts functions, classes, and import statements from Python source files.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::string_literals::harvest_string_literals;
use super::{ExtractionResult, Extractor};

/// Python tree-sitter extractor.
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
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_python::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let source = content.as_bytes();

        collect_nodes(tree.root_node(), path, source, &mut nodes, &mut edges);

        // Harvest string literals as synthetic Const nodes
        // Python: "string" node, strip quotes from raw text
        harvest_string_literals(
            tree.root_node(),
            path,
            source,
            "python",
            "string",
            None,
            &mut nodes,
        );

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
        "function_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                let name_str = name.utf8_text(source).unwrap_or("unknown").to_string();
                let body = node.utf8_text(source).unwrap_or("").to_string();
                let signature = extract_python_signature(&body);
                // Record the AST-accurate byte column of the name identifier so the
                // LSP enricher can place the cursor without signature string searching.
                let mut metadata = BTreeMap::new();
                metadata.insert("name_col".to_string(), name.start_position().column.to_string());

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name_str,
                        kind: NodeKind::Function,
                    },
                    language: "python".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature,
                    body,
                    metadata,
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        "class_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                let name_str = name.utf8_text(source).unwrap_or("unknown").to_string();
                let body = node.utf8_text(source).unwrap_or("").to_string();
                let signature = extract_python_signature(&body);
                // Record the AST-accurate byte column of the name identifier.
                let mut metadata = BTreeMap::new();
                metadata.insert("name_col".to_string(), name.start_position().column.to_string());

                nodes.push(Node {
                    id: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name_str,
                        kind: NodeKind::Struct, // class -> Struct in our model
                    },
                    language: "python".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature,
                    body,
                    metadata,
                    source: ExtractionSource::TreeSitter,
                });
            }
        }
        "expression_statement" => {
            // Module-level ALL_CAPS assignments like MAX_RETRIES = 5
            // The expression_statement wraps an assignment node
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if child.kind() == "assignment" {
                        if let Some(lhs) = child.child_by_field_name("left") {
                            let name_str = lhs.utf8_text(source).unwrap_or("").trim().to_string();
                            // Only ALL_CAPS identifiers (Python constant convention)
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
                                    // Only store simple scalar values
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
        "import_statement" | "import_from_statement" => {
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();
            let target = parse_python_import_target(&text);

            let import_node = Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: text.clone(),
                    kind: NodeKind::Import,
                },
                language: "python".to_string(),
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
        _ => {}
    }

    // Recurse into children
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_nodes(child, path, source, nodes, edges);
        }
    }
}

/// Extract signature from Python function/class definition.
fn extract_python_signature(body: &str) -> String {
    // Take the first line (the def/class line)
    body.lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches(':')
        .trim()
        .to_string()
}

/// Parse the module name from a Python import statement.
fn parse_python_import_target(text: &str) -> String {
    if text.starts_with("from ") {
        // "from foo.bar import baz" -> "foo.bar"
        text.strip_prefix("from ")
            .and_then(|s| s.split_whitespace().next())
            .unwrap_or("")
            .to_string()
    } else if text.starts_with("import ") {
        // "import foo.bar" -> "foo.bar"
        text.strip_prefix("import ")
            .and_then(|s| s.split_whitespace().next())
            .map(|s| s.trim_end_matches(','))
            .unwrap_or("")
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
        assert!(
            names.contains(&"__init__"),
            "Should find __init__ method"
        );
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

        let imports: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Import)
            .collect();
        assert_eq!(imports.len(), 3, "Should find 3 imports");

        let dep_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert_eq!(dep_edges.len(), 3, "Should produce 3 DependsOn edges");
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
        let class_node = result
            .nodes
            .iter()
            .find(|n| n.id.name == "Foo")
            .expect("Should find Foo");
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
        // 2 declared ALL_CAPS consts + synthetic string literals from string values in the code
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
