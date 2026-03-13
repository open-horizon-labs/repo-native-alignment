//! JavaScript tree-sitter extractor.
//!
//! Generic path: functions, generator functions, methods, classes, string literals.
//! Special cases: lexical_declaration (const/let/var with arrow-function detection),
//! import_statement, class property arrow functions.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::configs::JAVASCRIPT_CONFIG;
use super::generic::{count_branches, GenericExtractor};
use super::{ExtractionResult, Extractor};

pub struct JavaScriptExtractor;

impl JavaScriptExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for JavaScriptExtractor {
    fn extensions(&self) -> &[&str] {
        &["js", "jsx", "mjs", "cjs"]
    }

    fn name(&self) -> &str {
        "javascript-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut result = GenericExtractor::new(&JAVASCRIPT_CONFIG).run(path, content)?;

        // JavaScript-specific: lexical_declaration, import_statement.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_javascript::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            collect_js_specials(
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

/// Node kinds whose value child indicates a function binding.
const FUNCTION_VALUE_KINDS: &[&str] = &["arrow_function", "function_expression", "function"];

/// Check if a tree-sitter node kind represents a function expression.
fn is_function_value(kind: &str) -> bool {
    FUNCTION_VALUE_KINDS.contains(&kind)
}

/// Build a function-style signature for an arrow function or function expression
/// assigned to a variable.
fn build_arrow_signature(
    decl_keyword: &str,
    name: &str,
    value_node: tree_sitter::Node,
    source: &[u8],
) -> String {
    let value_kind = value_node.kind();

    let params = value_node
        .child_by_field_name("parameters")
        .and_then(|p| p.utf8_text(source).ok())
        .unwrap_or("()");

    let is_async = value_node
        .child(0)
        .map(|c| c.kind() == "async")
        .unwrap_or(false);
    let async_prefix = if is_async { "async " } else { "" };

    if value_kind == "arrow_function" {
        format!("{} {} = {}{} =>", decl_keyword, name, async_prefix, params)
    } else {
        // function expression
        format!(
            "{} {} = {}function{}",
            decl_keyword, name, async_prefix, params
        )
    }
}

/// Walk AST for JavaScript-specific nodes not handled by the generic extractor:
/// `lexical_declaration` (const/let/var), `import_statement`.
///
/// Detects arrow functions and function expressions in variable declarations
/// and emits `NodeKind::Function` for them.
fn collect_js_specials(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    match kind_str {
        // Handle export statements wrapping lexical declarations
        "export_statement" => {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    collect_js_specials(child, path, source, nodes, edges);
                }
            }
            return;
        }
        "lexical_declaration" | "variable_declaration" => {
            let decl_text = node.utf8_text(source).unwrap_or("").trim().to_string();

            let decl_keyword = if decl_text.starts_with("const ") {
                "const"
            } else if decl_text.starts_with("let ") {
                "let"
            } else if decl_text.starts_with("var ") {
                "var"
            } else {
                return;
            };

            for i in 0..node.child_count() {
                if let Some(decl) = node.child(i as u32) {
                    if decl.kind() == "variable_declarator" {
                        if let Some(name_node) = decl.child_by_field_name("name") {
                            let name_str =
                                name_node.utf8_text(source).unwrap_or("unknown").trim().to_string();
                            if name_str.starts_with('{') || name_str.starts_with('[') {
                                continue;
                            }

                            let value_node = decl.child_by_field_name("value");
                            let is_fn = value_node
                                .as_ref()
                                .map(|v| is_function_value(v.kind()))
                                .unwrap_or(false);

                            if is_fn {
                                let value_n = value_node.unwrap();
                                let body = node.utf8_text(source).unwrap_or("").to_string();
                                let signature =
                                    build_arrow_signature(decl_keyword, &name_str, value_n, source);

                                let mut metadata = BTreeMap::new();
                                metadata.insert(
                                    "name_col".to_string(),
                                    name_node.start_position().column.to_string(),
                                );

                                // Cyclomatic complexity
                                if !JAVASCRIPT_CONFIG.branch_node_types.is_empty() {
                                    let branches =
                                        count_branches(value_n, source, &JAVASCRIPT_CONFIG, true);
                                    metadata.insert(
                                        "cyclomatic".to_string(),
                                        (1 + branches).to_string(),
                                    );
                                }

                                nodes.push(Node {
                                    id: NodeId {
                                        root: String::new(),
                                        file: path.to_path_buf(),
                                        name: name_str,
                                        kind: NodeKind::Function,
                                    },
                                    language: "javascript".to_string(),
                                    line_start: node.start_position().row + 1,
                                    line_end: node.end_position().row + 1,
                                    signature,
                                    body,
                                    metadata,
                                    source: ExtractionSource::TreeSitter,
                                });
                            } else {
                                // Scalar const -- preserve existing behavior (only for `const`)
                                if decl_keyword == "const" {
                                    let value_str = value_node
                                        .and_then(|v| v.utf8_text(source).ok())
                                        .map(|s| s.trim().to_string());
                                    let signature =
                                        decl_text.lines().next().unwrap_or("").trim().to_string();
                                    let mut metadata = BTreeMap::new();
                                    metadata.insert(
                                        "name_col".to_string(),
                                        name_node.start_position().column.to_string(),
                                    );
                                    if let Some(ref v) = value_str {
                                        let is_scalar = v.starts_with('"')
                                            || v.starts_with('\'')
                                            || v.starts_with('`')
                                            || v.parse::<f64>().is_ok()
                                            || v == "true"
                                            || v == "false";
                                        if is_scalar {
                                            let stripped = v
                                                .trim_matches('"')
                                                .trim_matches('\'')
                                                .trim_matches('`');
                                            metadata.insert(
                                                "value".to_string(),
                                                stripped.to_string(),
                                            );
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
                                        language: "javascript".to_string(),
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
        }
        "import_statement" => {
            let text = node.utf8_text(source).unwrap_or("").trim().to_string();
            let source_node = node.child_by_field_name("source");
            let target = source_node
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();

            let import_node = Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: text.clone(),
                    kind: NodeKind::Import,
                },
                language: "javascript".to_string(),
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
            return; // don't recurse into import statements
        }
        // Class property arrow functions: `class Foo { handler = (x) => x }`
        "field_definition" => {
            let value_node = node.child_by_field_name("value");
            let is_fn = value_node
                .as_ref()
                .map(|v| is_function_value(v.kind()))
                .unwrap_or(false);

            if is_fn {
                let value_n = value_node.unwrap();
                // In JS tree-sitter, class fields use "property" for the name
                let name_node = node.child_by_field_name("property");
                if let Some(name_n) = name_node {
                    let name_str =
                        name_n.utf8_text(source).unwrap_or("unknown").trim().to_string();
                    let body = node.utf8_text(source).unwrap_or("").to_string();

                    let params = value_n
                        .child_by_field_name("parameters")
                        .and_then(|p| p.utf8_text(source).ok())
                        .unwrap_or("()");
                    let is_async = value_n
                        .child(0)
                        .map(|c| c.kind() == "async")
                        .unwrap_or(false);
                    let async_prefix = if is_async { "async " } else { "" };
                    let signature = format!("{} = {}{} =>", name_str, async_prefix, params);

                    let mut metadata = BTreeMap::new();
                    metadata.insert(
                        "name_col".to_string(),
                        name_n.start_position().column.to_string(),
                    );

                    if !JAVASCRIPT_CONFIG.branch_node_types.is_empty() {
                        let branches =
                            count_branches(value_n, source, &JAVASCRIPT_CONFIG, true);
                        metadata.insert("cyclomatic".to_string(), (1 + branches).to_string());
                    }

                    // Find parent class name
                    if let Some(class_node) = find_ancestor_class(node) {
                        if let Some(class_name_node) = class_node.child_by_field_name("name") {
                            if let Ok(class_name) = class_name_node.utf8_text(source) {
                                metadata.insert(
                                    "parent_scope".to_string(),
                                    class_name.to_string(),
                                );
                            }
                        }
                    }

                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name: name_str,
                            kind: NodeKind::Function,
                        },
                        language: "javascript".to_string(),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        signature,
                        body,
                        metadata,
                        source: ExtractionSource::TreeSitter,
                    });
                }
            }
        }
        _ => {}
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_js_specials(child, path, source, nodes, edges);
        }
    }
}

/// Walk up the tree to find the nearest class_declaration or class ancestor.
fn find_ancestor_class(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "class_declaration" || n.kind() == "class" {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_const_arrow_function_indexed_as_function() {
        let extractor = JavaScriptExtractor::new();
        let code = r#"
const handler = (req, res) => {
    res.send("hello");
};
"#;
        let result = extractor.extract(Path::new("src/app.js"), code).unwrap();

        let handler = result
            .nodes
            .iter()
            .find(|n| n.id.name == "handler")
            .expect("Should find handler");
        assert_eq!(
            handler.id.kind,
            NodeKind::Function,
            "Arrow function should be Function, not Const"
        );
        assert!(
            handler.signature.contains("handler"),
            "Signature should contain the name"
        );
        assert!(
            handler.signature.contains("=>"),
            "Signature should contain =>"
        );
    }

    #[test]
    fn test_function_expression_indexed_as_function() {
        let extractor = JavaScriptExtractor::new();
        let code = r#"
const transform = function(x) { return x + 1; };
"#;
        let result = extractor.extract(Path::new("src/app.js"), code).unwrap();

        let transform = result
            .nodes
            .iter()
            .find(|n| n.id.name == "transform")
            .expect("Should find transform");
        assert_eq!(
            transform.id.kind,
            NodeKind::Function,
            "Function expression should be Function, not Const"
        );
    }

    #[test]
    fn test_exported_arrow_function() {
        let extractor = JavaScriptExtractor::new();
        let code = r#"
export const middleware = async (ctx) => {
    await ctx.next();
};
"#;
        let result = extractor.extract(Path::new("src/app.js"), code).unwrap();

        let mw = result
            .nodes
            .iter()
            .find(|n| n.id.name == "middleware")
            .expect("Should find middleware");
        assert_eq!(
            mw.id.kind,
            NodeKind::Function,
            "Exported arrow function should be Function"
        );
        assert!(
            mw.signature.contains("async"),
            "Signature should reflect async: {}",
            mw.signature
        );
    }

    #[test]
    fn test_arrow_function_cyclomatic_complexity() {
        let extractor = JavaScriptExtractor::new();
        let code = r#"
const validate = (x) => {
    if (x > 0) {
        if (x < 100) {
            return true;
        }
    }
    return false;
};
"#;
        let result = extractor.extract(Path::new("src/app.js"), code).unwrap();

        let validate = result
            .nodes
            .iter()
            .find(|n| n.id.name == "validate")
            .expect("Should find validate");
        assert_eq!(validate.id.kind, NodeKind::Function);
        let complexity: usize = validate
            .metadata
            .get("cyclomatic")
            .expect("Should have cyclomatic complexity")
            .parse()
            .unwrap();
        assert!(
            complexity >= 3,
            "Two if statements = complexity >= 3, got {}",
            complexity
        );
    }

    #[test]
    fn test_let_var_arrow_functions() {
        let extractor = JavaScriptExtractor::new();
        let code = r#"
let onChange = (e) => { console.log(e); };
var legacy = (x) => x;
"#;
        let result = extractor.extract(Path::new("src/app.js"), code).unwrap();

        let on_change = result
            .nodes
            .iter()
            .find(|n| n.id.name == "onChange")
            .expect("Should find onChange (let)");
        assert_eq!(on_change.id.kind, NodeKind::Function);

        let legacy = result
            .nodes
            .iter()
            .find(|n| n.id.name == "legacy")
            .expect("Should find legacy (var)");
        assert_eq!(legacy.id.kind, NodeKind::Function);
    }

    #[test]
    fn test_scalar_const_still_const() {
        let extractor = JavaScriptExtractor::new();
        let code = r#"
const PORT = 3000;
const NAME = "hello";
const ENABLED = true;
"#;
        let result = extractor.extract(Path::new("src/app.js"), code).unwrap();

        for name in &["PORT", "NAME", "ENABLED"] {
            let node = result
                .nodes
                .iter()
                .find(|n| n.id.name == *name)
                .unwrap_or_else(|| panic!("Should find const {}", name));
            assert_eq!(
                node.id.kind,
                NodeKind::Const,
                "{} should remain Const",
                name
            );
        }
    }

    #[test]
    fn test_class_property_arrow_function() {
        let extractor = JavaScriptExtractor::new();
        let code = r#"
class Foo {
    handler = (x) => x * 2;
    onClick = async (e) => {
        e.preventDefault();
    };
}
"#;
        let result = extractor.extract(Path::new("src/app.js"), code).unwrap();

        let handler = result
            .nodes
            .iter()
            .find(|n| n.id.name == "handler" && n.id.kind == NodeKind::Function)
            .expect("Should find handler as Function");
        assert_eq!(handler.id.kind, NodeKind::Function);

        let on_click = result
            .nodes
            .iter()
            .find(|n| n.id.name == "onClick" && n.id.kind == NodeKind::Function)
            .expect("Should find onClick as Function");
        assert_eq!(on_click.id.kind, NodeKind::Function);
    }
}
