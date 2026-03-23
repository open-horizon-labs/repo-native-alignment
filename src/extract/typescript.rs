//! TypeScript tree-sitter extractor.
//!
//! Generic path: functions, methods, classes, interfaces, enums, fields, string literals.
//! Special cases: lexical_declaration (const/let/var with arrow-function detection),
//! import_statement, type_alias_declaration, class property arrow functions.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::configs::TYPESCRIPT_CONFIG;
use super::generic::{count_branches, GenericExtractor};
use super::{ExtractionResult, Extractor};

/// TypeScript tree-sitter extractor (handles .ts and .tsx files).
pub struct TypeScriptExtractor;

impl Default for TypeScriptExtractor {
    fn default() -> Self {
        Self::new()
    }
}

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

        // TypeScript-specific: lexical_declaration, import_statement, type_alias_declaration.
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

/// Node kinds whose value child indicates a function binding.
const FUNCTION_VALUE_KINDS: &[&str] = &["arrow_function", "function_expression", "function"];

/// Check if a tree-sitter node kind represents a function expression.
fn is_function_value(kind: &str) -> bool {
    FUNCTION_VALUE_KINDS.contains(&kind)
}

/// Extract a function-style signature from a variable declaration with an
/// arrow function or function expression value.
///
/// Produces signatures like:
/// - `const handler = (req: Request, res: Response) =>`
/// - `const transform = function(x)`
/// - `let cb = async (ctx) =>`
fn build_arrow_signature(
    decl_keyword: &str,
    name: &str,
    value_node: tree_sitter::Node,
    source: &[u8],
) -> String {
    let value_kind = value_node.kind();

    // Extract the parameters text from the function value
    let params = value_node
        .child_by_field_name("parameters")
        .and_then(|p| p.utf8_text(source).ok())
        .unwrap_or("()");

    // Check for async prefix
    let is_async = value_node
        .child(0)
        .map(|c| c.kind() == "async")
        .unwrap_or(false);

    let async_prefix = if is_async { "async " } else { "" };

    // Check for return type annotation (TypeScript)
    let return_type = value_node
        .child_by_field_name("return_type")
        .and_then(|rt| rt.utf8_text(source).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    if value_kind == "arrow_function" {
        if return_type.is_empty() {
            format!("{} {} = {}{} =>", decl_keyword, name, async_prefix, params)
        } else {
            format!(
                "{} {} = {}{}{} =>",
                decl_keyword, name, async_prefix, params, return_type
            )
        }
    } else {
        // function expression
        if return_type.is_empty() {
            format!(
                "{} {} = {}function{}",
                decl_keyword, name, async_prefix, params
            )
        } else {
            format!(
                "{} {} = {}function{}{}",
                decl_keyword, name, async_prefix, params, return_type
            )
        }
    }
}

/// Emit a module-level `Defines` edge: `<file_stem>:Module -> <name>:<kind>`.
///
/// Mirrors the edge that the generic extractor emits for top-level symbols.
/// Arrow functions and function expressions bypass the generic path and need
/// this edge explicitly.
fn emit_module_defines_edge(path: &Path, name: &str, kind: NodeKind, edges: &mut Vec<Edge>) {
    let module_id = NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: path
            .file_stem()
            .unwrap_or(std::ffi::OsStr::new("module"))
            .to_string_lossy()
            .to_string(),
        kind: NodeKind::Module,
    };
    edges.push(Edge {
        from: module_id,
        to: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: name.to_string(),
            kind,
        },
        kind: EdgeKind::Defines,
        source: ExtractionSource::TreeSitter,
        confidence: Confidence::Detected,
    });
}

/// Emit a class-level `Defines` edge: `<class_name>:Struct -> <name>:<kind>`.
fn emit_class_defines_edge(
    path: &Path,
    class_name: &str,
    member_name: &str,
    member_kind: NodeKind,
    edges: &mut Vec<Edge>,
) {
    edges.push(Edge {
        from: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: class_name.to_string(),
            kind: NodeKind::Struct,
        },
        to: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: member_name.to_string(),
            kind: member_kind,
        },
        kind: EdgeKind::Defines,
        source: ExtractionSource::TreeSitter,
        confidence: Confidence::Detected,
    });
}

/// Extract DependsOn edges for parameter types and return type from a function
/// value node (arrow_function or function_expression) in TypeScript.
///
/// TypeScript has typed parameters and return types that produce DependsOn edges
/// in the generic extractor. Arrow functions bypass that path, so we extract
/// them here.
fn emit_ts_type_edges(
    path: &Path,
    fn_name: &str,
    value_node: tree_sitter::Node,
    source: &[u8],
    edges: &mut Vec<Edge>,
) {
    use super::generic::extract_user_type;

    let fn_id = NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: fn_name.to_string(),
        kind: NodeKind::Function,
    };

    // Parameter types: walk formal_parameters children looking for type annotations
    if let Some(params) = value_node.child_by_field_name("parameters") {
        for i in 0..params.child_count() {
            if let Some(param) = params.child(i as u32)
                && let Some(tn) = param.child_by_field_name("type") {
                    let type_text = tn.utf8_text(source).unwrap_or("");
                    // type_requires_uppercase = true for TypeScript
                    if let Some(type_name) = extract_user_type(type_text, true) {
                        edges.push(Edge {
                            from: fn_id.clone(),
                            to: NodeId {
                                root: String::new(),
                                file: path.to_path_buf(),
                                name: type_name,
                                kind: NodeKind::Struct,
                            },
                            kind: EdgeKind::DependsOn,
                            source: ExtractionSource::TreeSitter,
                            confidence: Confidence::Detected,
                        });
                    }
                }
        }
    }

    // Return type
    if let Some(rn) = value_node.child_by_field_name("return_type") {
        let ret_text = rn.utf8_text(source).unwrap_or("");
        if let Some(type_name) = extract_user_type(ret_text, true) {
            edges.push(Edge {
                from: fn_id,
                to: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: type_name,
                    kind: NodeKind::Struct,
                },
                kind: EdgeKind::DependsOn,
                source: ExtractionSource::TreeSitter,
                confidence: Confidence::Detected,
            });
        }
    }
}

/// Walk AST for TypeScript-specific nodes not handled by the generic extractor:
/// `lexical_declaration` (const/let/var), `import_statement`, `type_alias_declaration`.
///
/// For `lexical_declaration`: inspects variable_declarator value children to detect
/// arrow functions and function expressions, emitting `NodeKind::Function` for those
/// and `NodeKind::Const` for scalar values. Also emits module-level `Defines` edges
/// and parameter/return-type `DependsOn` edges that the generic path would normally produce.
fn collect_ts_specials(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    match kind_str {
        // Handle export statements that wrap lexical declarations:
        // `export const handler = (req) => { ... }`
        "export_statement" => {
            // Process the declaration child (if any) through the normal path
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    collect_ts_specials(child, path, source, nodes, edges);
                }
            }
            return; // don't recurse again below
        }
        "lexical_declaration" | "variable_declaration" => {
            let decl_text = node.utf8_text(source).unwrap_or("").trim().to_string();

            // Determine declaration keyword (const, let, var)
            let decl_keyword = if decl_text.starts_with("const ") {
                "const"
            } else if decl_text.starts_with("let ") {
                "let"
            } else if decl_text.starts_with("var ") {
                "var"
            } else {
                // Unknown declaration kind, skip
                return;
            };

            for i in 0..node.child_count() {
                if let Some(decl) = node.child(i as u32)
                    && decl.kind() == "variable_declarator"
                        && let Some(name_node) = decl.child_by_field_name("name") {
                            let name_str =
                                name_node.utf8_text(source).unwrap_or("unknown").trim().to_string();
                            // Skip destructuring patterns
                            if name_str.starts_with('{') || name_str.starts_with('[') {
                                continue;
                            }

                            // Check if value is an arrow function or function expression
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

                                // Cyclomatic complexity for arrow/function expressions
                                if !TYPESCRIPT_CONFIG.branch_node_types.is_empty() {
                                    let branches =
                                        count_branches(value_n, source, &TYPESCRIPT_CONFIG, true);
                                    metadata.insert(
                                        "cyclomatic".to_string(),
                                        (1 + branches).to_string(),
                                    );
                                }

                                nodes.push(Node {
                                    id: NodeId {
                                        root: String::new(),
                                        file: path.to_path_buf(),
                                        name: name_str.clone(),
                                        kind: NodeKind::Function,
                                    },
                                    language: "typescript".to_string(),
                                    line_start: node.start_position().row + 1,
                                    line_end: node.end_position().row + 1,
                                    signature,
                                    body,
                                    metadata,
                                    source: ExtractionSource::TreeSitter,
                                });

                                // Emit module-level Defines edge (mirrors generic extractor)
                                emit_module_defines_edge(
                                    path,
                                    &name_str,
                                    NodeKind::Function,
                                    edges,
                                );

                                // Emit DependsOn edges for parameter/return types
                                emit_ts_type_edges(path, &name_str, value_n, source, edges);
                            } else {
                                // Scalar const — preserve existing behavior (only for `const`)
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
                                        let stripped = v.trim_matches('"').trim_matches('\'');
                                        let is_scalar = v.starts_with('"')
                                            || v.starts_with('\'')
                                            || v.parse::<f64>().is_ok()
                                            || v == "true"
                                            || v == "false";
                                        if is_scalar {
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
                                            name: name_str.clone(),
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

                                    // Emit module-level Defines edge for const nodes
                                    emit_module_defines_edge(
                                        path,
                                        &name_str,
                                        NodeKind::Const,
                                        edges,
                                    );
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

            // Emit module-level Defines edge for import nodes
            emit_module_defines_edge(
                path,
                &import_node.id.name,
                NodeKind::Import,
                edges,
            );

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
                        name: name_str.clone(),
                        kind: NodeKind::TypeAlias,
                    },
                    language: "typescript".to_string(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature: body.clone(),
                    body,
                    metadata,
                    source: ExtractionSource::TreeSitter,
                });

                // Emit module-level Defines edge for type alias nodes
                emit_module_defines_edge(
                    path,
                    &name_str,
                    NodeKind::TypeAlias,
                    edges,
                );
            }
        }
        // Enum variants: `enum Direction { Up, Down = 1 }`
        // TS tree-sitter uses `property_identifier` for plain members and
        // `enum_assignment` for initialized members (which contain a `property_identifier`).
        // The generic extractor doesn't handle these because `property_identifier` is too
        // generic to add to node_kinds.
        "enum_body" => {
            // Find the parent enum_declaration to get the enum name
            let enum_name = node.parent()
                .and_then(|p| p.child_by_field_name("name"))
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("unknown")
                .to_string();

            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    let variant_name = match child.kind() {
                        "property_identifier" => {
                            child.utf8_text(source).ok().map(|s| s.to_string())
                        }
                        "enum_assignment" => {
                            // The name is the first `property_identifier` child
                            child.child(0)
                                .filter(|c| c.kind() == "property_identifier")
                                .and_then(|c| c.utf8_text(source).ok())
                                .map(|s| s.to_string())
                        }
                        _ => None,
                    };

                    if let Some(name) = variant_name {
                        let body = child.utf8_text(source).unwrap_or("").to_string();
                        let mut metadata = BTreeMap::new();
                        metadata.insert("parent_scope".to_string(), enum_name.clone());
                        // name_col for cursor positioning (consistent with generic extractor)
                        let col = if child.kind() == "property_identifier" {
                            child.start_position().column
                        } else {
                            // enum_assignment: use the property_identifier child's column
                            child.child(0)
                                .filter(|c| c.kind() == "property_identifier")
                                .map(|c| c.start_position().column)
                                .unwrap_or(child.start_position().column)
                        };
                        metadata.insert("name_col".to_string(), col.to_string());

                        nodes.push(Node {
                            id: NodeId {
                                root: String::new(),
                                file: path.to_path_buf(),
                                name: name.clone(),
                                kind: NodeKind::Field,
                            },
                            language: "typescript".to_string(),
                            line_start: child.start_position().row + 1,
                            line_end: child.end_position().row + 1,
                            signature: body.clone(),
                            body,
                            metadata,
                            source: ExtractionSource::TreeSitter,
                        });

                        // Emit HasField edge: Direction:Enum -> Up:Field
                        edges.push(Edge {
                            from: NodeId {
                                root: String::new(),
                                file: path.to_path_buf(),
                                name: enum_name.clone(),
                                kind: NodeKind::Enum,
                            },
                            to: NodeId {
                                root: String::new(),
                                file: path.to_path_buf(),
                                name,
                                kind: NodeKind::Field,
                            },
                            kind: EdgeKind::HasField,
                            source: ExtractionSource::TreeSitter,
                            confidence: Confidence::Detected,
                        });
                    }
                }
            }
            return; // don't recurse further into enum_body
        }
        // Class property arrow functions: `class Foo { handler = (x) => x }`
        // The generic extractor sees `public_field_definition` as `NodeKind::Field`.
        // We detect arrow function values and upgrade to Function, emitting the
        // class-level Defines edge and type DependsOn edges.
        "public_field_definition" => {
            let value_node = node.child_by_field_name("value");
            let is_fn = value_node
                .as_ref()
                .map(|v| is_function_value(v.kind()))
                .unwrap_or(false);

            if is_fn {
                let value_n = value_node.unwrap();
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name_str =
                        name_node.utf8_text(source).unwrap_or("unknown").trim().to_string();
                    let body = node.utf8_text(source).unwrap_or("").to_string();

                    // Build signature for class property — use appropriate style
                    let params = value_n
                        .child_by_field_name("parameters")
                        .and_then(|p| p.utf8_text(source).ok())
                        .unwrap_or("()");
                    let is_async = value_n
                        .child(0)
                        .map(|c| c.kind() == "async")
                        .unwrap_or(false);
                    let async_prefix = if is_async { "async " } else { "" };

                    let return_type = value_n
                        .child_by_field_name("return_type")
                        .and_then(|rt| rt.utf8_text(source).ok())
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default();

                    let signature = if value_n.kind() == "arrow_function" {
                        if return_type.is_empty() {
                            format!("{} = {}{} =>", name_str, async_prefix, params)
                        } else {
                            format!("{} = {}{}{} =>", name_str, async_prefix, params, return_type)
                        }
                    } else {
                        // function_expression
                        if return_type.is_empty() {
                            format!("{} = {}function{}", name_str, async_prefix, params)
                        } else {
                            format!(
                                "{} = {}function{}{}",
                                name_str, async_prefix, params, return_type
                            )
                        }
                    };

                    let mut metadata = BTreeMap::new();
                    metadata.insert(
                        "name_col".to_string(),
                        name_node.start_position().column.to_string(),
                    );

                    // Cyclomatic complexity
                    if !TYPESCRIPT_CONFIG.branch_node_types.is_empty() {
                        let branches =
                            count_branches(value_n, source, &TYPESCRIPT_CONFIG, true);
                        metadata.insert("cyclomatic".to_string(), (1 + branches).to_string());
                    }

                    // Determine parent scope (class name) and emit Defines edge
                    if let Some(class_node) = find_ancestor_class(node)
                        && let Some(class_name_node) = class_node.child_by_field_name("name")
                            && let Ok(class_name) = class_name_node.utf8_text(source) {
                                metadata.insert(
                                    "parent_scope".to_string(),
                                    class_name.to_string(),
                                );
                                emit_class_defines_edge(
                                    path,
                                    class_name,
                                    &name_str,
                                    NodeKind::Function,
                                    edges,
                                );
                            }

                    // Emit DependsOn edges for parameter/return types
                    emit_ts_type_edges(path, &name_str, value_n, source, edges);

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
                        metadata,
                        source: ExtractionSource::TreeSitter,
                    });
                }
            }
            // If not a function value, the generic extractor already handles it as Field
        }
        _ => {}
    }

    // Recurse into children (except for export_statement which handles its own recursion above)
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_ts_specials(child, path, source, nodes, edges);
        }
    }
}

/// Walk up the tree to find the nearest class_declaration ancestor.
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

    // --- Arrow function / function expression tests ---

    #[test]
    fn test_const_arrow_function_indexed_as_function() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
const handler = (req: Request, res: Response) => {
    res.send("hello");
};
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

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
        let extractor = TypeScriptExtractor::new();
        let code = r#"
const transform = function(x: number): number { return x + 1; };
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

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
        let extractor = TypeScriptExtractor::new();
        let code = r#"
export const middleware = async (ctx: Context) => {
    await ctx.next();
};
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

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
        let extractor = TypeScriptExtractor::new();
        let code = r#"
const validate = (x: number) => {
    if (x > 0) {
        if (x < 100) {
            return true;
        }
    }
    return false;
};
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

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
        let extractor = TypeScriptExtractor::new();
        let code = r#"
let onChange = (e: Event) => { console.log(e); };
var legacy = (x: any) => x;
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

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
        let extractor = TypeScriptExtractor::new();
        let code = r#"
const PORT = 3000;
const NAME = "hello";
const ENABLED = true;
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

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
        let extractor = TypeScriptExtractor::new();
        let code = r#"
class Foo {
    handler = (x: number) => x * 2;
    onClick = async (e: Event) => {
        e.preventDefault();
    };
}
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

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

    // --- Edge tests (CodeRabbit findings #2, #3, #4) ---

    #[test]
    fn test_arrow_function_gets_module_defines_edge() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
const handler = (req: Request, res: Response) => {
    res.send("hello");
};
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

        let defines_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::Defines
                    && e.from.kind == NodeKind::Module
                    && e.to.name == "handler"
                    && e.to.kind == NodeKind::Function
            })
            .collect();
        assert_eq!(
            defines_edges.len(),
            1,
            "Arrow function should have module-level Defines edge"
        );
    }

    #[test]
    fn test_arrow_function_gets_depends_on_edges_for_types() {
        // Finding #3: TS arrow functions should emit DependsOn edges for param/return types
        let extractor = TypeScriptExtractor::new();
        let code = r#"
const handler = (req: Request, res: Response): Result => {
    res.send("hello");
};
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

        let depends_on: Vec<_> = result
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::DependsOn
                    && e.from.name == "handler"
                    && e.from.kind == NodeKind::Function
            })
            .collect();

        let type_names: Vec<&str> = depends_on.iter().map(|e| e.to.name.as_str()).collect();
        assert!(
            type_names.contains(&"Request"),
            "Should have DependsOn edge for Request param type, got: {:?}",
            type_names
        );
        assert!(
            type_names.contains(&"Response"),
            "Should have DependsOn edge for Response param type, got: {:?}",
            type_names
        );
    }

    #[test]
    fn test_class_property_arrow_gets_defines_edge() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
class Foo {
    handler = (x: number) => x * 2;
}
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

        let class_defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::Defines
                    && e.from.name == "Foo"
                    && e.from.kind == NodeKind::Struct
                    && e.to.name == "handler"
                    && e.to.kind == NodeKind::Function
            })
            .collect();
        assert_eq!(
            class_defines.len(),
            1,
            "Class property arrow should have Defines edge from class"
        );
    }

    #[test]
    fn test_class_property_function_expression_signature() {
        // Finding #4: class field with function_expression should not use arrow signature
        let extractor = TypeScriptExtractor::new();
        let code = r#"
class Foo {
    handler = function(x: number): number { return x * 2; };
}
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

        let handler = result
            .nodes
            .iter()
            .find(|n| n.id.name == "handler" && n.id.kind == NodeKind::Function)
            .expect("Should find handler as Function");
        assert!(
            handler.signature.contains("function"),
            "function_expression class property should have 'function' in signature, got: {}",
            handler.signature
        );
        assert!(
            !handler.signature.contains("=>"),
            "function_expression class property should NOT have '=>' in signature, got: {}",
            handler.signature
        );
    }

    // --- Defines edge tests for imports, consts, type aliases (#168) ---

    #[test]
    fn test_const_gets_module_defines_edge() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
const PORT = 3000;
const NAME = "hello";
"#;
        let result = extractor.extract(Path::new("src/config.ts"), code).unwrap();

        for name in &["PORT", "NAME"] {
            let defines: Vec<_> = result
                .edges
                .iter()
                .filter(|e| {
                    e.kind == EdgeKind::Defines
                        && e.from.kind == NodeKind::Module
                        && e.from.name == "config"
                        && e.to.name == *name
                        && e.to.kind == NodeKind::Const
                })
                .collect();
            assert_eq!(
                defines.len(),
                1,
                "Const {} should have module-level Defines edge",
                name,
            );
        }
    }

    #[test]
    fn test_import_gets_module_defines_edge() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
import { Router } from 'express';
import path from 'path';
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

        let import_defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::Defines
                    && e.from.kind == NodeKind::Module
                    && e.from.name == "app"
                    && e.to.kind == NodeKind::Import
            })
            .collect();
        assert_eq!(
            import_defines.len(),
            2,
            "Each import should have a module-level Defines edge",
        );
    }

    #[test]
    fn test_type_alias_uses_first_class_kind() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
type UserId = string;
type Config = { port: number; host: string };
type Handler = (req: Request) => Response;
"#;
        let result = extractor.extract(Path::new("src/types.ts"), code).unwrap();

        let type_aliases: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::TypeAlias)
            .collect();
        assert_eq!(type_aliases.len(), 3, "Should find 3 type aliases");

        let names: Vec<&str> = type_aliases.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"UserId"), "Should find type alias UserId");
        assert!(names.contains(&"Config"), "Should find type alias Config");
        assert!(names.contains(&"Handler"), "Should find type alias Handler");

        // Verify they are NOT using Other("type_alias")
        let others: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| matches!(&n.id.kind, NodeKind::Other(s) if s == "type_alias"))
            .collect();
        assert!(others.is_empty(), "Should not use Other(\"type_alias\") anymore");
    }

    #[test]
    fn test_type_alias_gets_module_defines_edge() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
type UserId = string;
type Config = { port: number; host: string };
"#;
        let result = extractor.extract(Path::new("src/types.ts"), code).unwrap();

        for name in &["UserId", "Config"] {
            let defines: Vec<_> = result
                .edges
                .iter()
                .filter(|e| {
                    e.kind == EdgeKind::Defines
                        && e.from.kind == NodeKind::Module
                        && e.from.name == "types"
                        && e.to.name == *name
                })
                .collect();
            assert_eq!(
                defines.len(),
                1,
                "Type alias {} should have module-level Defines edge",
                name,
            );
        }
    }

    #[test]
    fn test_all_special_nodes_reachable_from_module() {
        // Integration test: module graph traversal finds all special-cased nodes
        let extractor = TypeScriptExtractor::new();
        let code = r#"
import { Router } from 'express';
const PORT = 3000;
const handler = (req: Request, res: Response) => { res.send("ok"); };
type UserId = string;
function regularFn() {}
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

        let module_defines: Vec<String> = result
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::Defines
                    && e.from.kind == NodeKind::Module
                    && e.from.name == "app"
            })
            .map(|e| e.to.name.clone())
            .collect();

        // Import
        assert!(
            module_defines.iter().any(|n| n.contains("import")),
            "Module should define import node, defines: {:?}",
            module_defines
        );
        // Const
        assert!(
            module_defines.contains(&"PORT".to_string()),
            "Module should define PORT const, defines: {:?}",
            module_defines
        );
        // Arrow function
        assert!(
            module_defines.contains(&"handler".to_string()),
            "Module should define handler arrow fn, defines: {:?}",
            module_defines
        );
        // Type alias
        assert!(
            module_defines.contains(&"UserId".to_string()),
            "Module should define UserId type alias, defines: {:?}",
            module_defines
        );
        // Regular function (from generic extractor)
        assert!(
            module_defines.contains(&"regularFn".to_string()),
            "Module should define regularFn, defines: {:?}",
            module_defines
        );
    }

    // --- Adversarial tests seeded from dissent (#168) ---

    #[test]
    fn test_no_duplicate_defines_edges_for_const() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
const PORT = 3000;
"#;
        let result = extractor.extract(Path::new("src/config.ts"), code).unwrap();

        let defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::Defines
                    && e.to.name == "PORT"
            })
            .collect();
        assert_eq!(
            defines.len(),
            1,
            "Should have exactly 1 Defines edge for PORT, not duplicates",
        );
    }

    #[test]
    fn test_no_duplicate_defines_edges_for_import() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
import { Router } from 'express';
"#;
        let result = extractor.extract(Path::new("src/app.ts"), code).unwrap();

        let defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::Defines
                    && e.to.kind == NodeKind::Import
            })
            .collect();
        assert_eq!(
            defines.len(),
            1,
            "Should have exactly 1 Defines edge for import, not duplicates",
        );
    }

    #[test]
    fn test_exported_const_gets_defines_edge() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
export const API_KEY = "secret";
"#;
        let result = extractor.extract(Path::new("src/config.ts"), code).unwrap();

        let defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::Defines
                    && e.from.kind == NodeKind::Module
                    && e.to.name == "API_KEY"
                    && e.to.kind == NodeKind::Const
            })
            .collect();
        assert_eq!(
            defines.len(),
            1,
            "Exported const should have module-level Defines edge",
        );
    }

    #[test]
    fn test_exported_type_alias_gets_defines_edge() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
export type UserId = string;
"#;
        let result = extractor.extract(Path::new("src/types.ts"), code).unwrap();

        let defines: Vec<_> = result
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::Defines
                    && e.from.kind == NodeKind::Module
                    && e.to.name == "UserId"
            })
            .collect();
        assert_eq!(
            defines.len(),
            1,
            "Exported type alias should have module-level Defines edge",
        );
    }

    #[test]
    fn test_extract_ts_interface_method_signatures() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
interface Service {
    serve(port: number): void;
    stop(): Promise<void>;
}
"#;
        let result = extractor.extract(Path::new("src/service.ts"), code).unwrap();

        // Interface itself should be found as Trait
        let service = result.nodes.iter().find(|n| n.id.name == "Service" && n.id.kind == NodeKind::Trait);
        assert!(service.is_some(), "Should find interface Service");

        // Method signatures should be indexed as Function nodes
        let serve = result.nodes.iter().find(|n| n.id.name == "serve" && n.id.kind == NodeKind::Function);
        assert!(serve.is_some(), "Should find interface method serve");

        let stop = result.nodes.iter().find(|n| n.id.name == "stop" && n.id.kind == NodeKind::Function);
        assert!(stop.is_some(), "Should find interface method stop");

        // Methods should have parent_scope pointing to the interface
        assert_eq!(
            serve.unwrap().metadata.get("parent_scope"),
            Some(&"Service".to_string()),
            "serve should have parent_scope = Service"
        );
        assert_eq!(
            stop.unwrap().metadata.get("parent_scope"),
            Some(&"Service".to_string()),
            "stop should have parent_scope = Service"
        );
    }

    #[test]
    fn test_typescript_is_static() {
        let extractor = TypeScriptExtractor::new();
        let code = r#"
class MyService {
    static create(): MyService {
        return new MyService();
    }

    serve(): void {
        console.log("serving");
    }

    static count(): number {
        return 0;
    }
}
"#;
        let result = extractor.extract(Path::new("service.ts"), code).unwrap();

        let create = result.nodes.iter().find(|n| n.id.name == "create" && n.id.kind == NodeKind::Function).unwrap();
        assert_eq!(create.metadata.get("is_static").map(|s| s.as_str()), Some("true"), "static create() should be static");

        let serve = result.nodes.iter().find(|n| n.id.name == "serve" && n.id.kind == NodeKind::Function).unwrap();
        assert_eq!(serve.metadata.get("is_static").map(|s| s.as_str()), Some("false"), "serve() should be instance");

        let count = result.nodes.iter().find(|n| n.id.name == "count" && n.id.kind == NodeKind::Function).unwrap();
        assert_eq!(count.metadata.get("is_static").map(|s| s.as_str()), Some("true"), "static count() should be static");
    }

    #[test]
    fn test_typescript_top_level_fn_no_is_static() {
        let extractor = TypeScriptExtractor::new();
        let code = "function topLevel(): void {}\n";
        let result = extractor.extract(Path::new("app.ts"), code).unwrap();

        let func = result.nodes.iter().find(|n| n.id.name == "topLevel").unwrap();
        assert!(func.metadata.get("is_static").is_none(), "Top-level function should NOT have is_static");
    }
}
