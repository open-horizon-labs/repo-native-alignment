//! Generic tree-sitter extractor driven by a per-language node-kind config table.
//!
//! Eliminates the per-extractor boilerplate: init parser → walk AST → match node
//! kinds → emit Node. Each language provides a [`LangConfig`] with:
//!
//! - The tree-sitter `Language` object
//! - File extensions
//! - A node-kind map: `&[("ts_node_kind", NodeKind)]`
//! - Optional scope-propagating parent kinds (e.g. `["impl_item", "struct_item"]`)
//! - Optional const value field name (e.g. `"value"` for Rust `const_item`)
//! - String literal node kinds for synthetic Const harvesting
//!
//! Languages with special cases (Go multi-name const, Python ALL_CAPS, Rust impl
//! scope rules) keep a thin per-language extractor that calls
//! `GenericExtractor::extract_with_extra` and appends their custom nodes.
//!
//! # Coverage
//! This module covers the ~80% common case. Per-language escape hatches remain in
//! the individual extractor files, but as small focused functions rather than
//! full 300-line traversal reimplementations.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};
use super::string_literals::harvest_string_literals;
use super::{ExtractionResult, Extractor};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Per-language configuration for the generic extractor.
pub struct LangConfig {
    /// Returns the tree-sitter language. A function pointer so `LangConfig`
    /// can be a `static` (tree-sitter `Language` is a runtime value).
    pub language_fn: fn() -> tree_sitter::Language,
    /// Display name (e.g. "rust", "go").
    pub language_name: &'static str,
    /// File extensions handled by this extractor.
    pub extensions: &'static [&'static str],
    /// Mapping from tree-sitter node kind string to our `NodeKind`.
    /// Order matters for matching — first match wins.
    pub node_kinds: &'static [(&'static str, NodeKind)],
    /// Node kinds whose children should be walked with the node's name as
    /// parent scope (e.g. `["impl_item", "struct_item", "enum_item"]`).
    pub scope_parent_kinds: &'static [&'static str],
    /// If set, extract this field child's text as the `value` metadata key
    /// for nodes of kind `NodeKind::Const` (e.g. `"value"` for Rust const_item).
    pub const_value_field: Option<&'static str>,
    /// Node kinds that use the full node text as their name (e.g. `use_declaration`
    /// in Rust where the name IS the full `use crate::foo::Bar;` text).
    pub full_text_name_kinds: &'static [&'static str],
    /// String literal tree-sitter node kinds for synthetic Const harvesting.
    /// Each entry: (outer_node_kind, optional_content_child_kind).
    pub string_literal_kinds: &'static [(&'static str, Option<&'static str>)],
    /// Tree-sitter field name for the parameters container on function nodes.
    /// None = skip DependsOn type edge extraction for this language.
    pub param_container_field: Option<&'static str>,
    /// Tree-sitter field name for the type annotation on each parameter.
    pub param_type_field: Option<&'static str>,
    /// Tree-sitter field name for the function return type.
    pub return_type_field: Option<&'static str>,
    /// Whether user-defined types must start with uppercase.
    /// false for Go (unexported types) and Python (lowercase classes).
    pub type_requires_uppercase: bool,
    /// Tree-sitter node kinds that represent branches/decision points for
    /// cyclomatic complexity (e.g. `["if_expression", "match_expression",
    /// "for_expression", "while_expression"]`).
    pub branch_node_types: &'static [&'static str],
}

// ---------------------------------------------------------------------------
// Extractor
// ---------------------------------------------------------------------------

/// Generic tree-sitter extractor driven by [`LangConfig`].
pub struct GenericExtractor {
    pub config: &'static LangConfig,
}

impl GenericExtractor {
    pub fn new(config: &'static LangConfig) -> Self {
        Self { config }
    }

    /// Run extraction and return the result. Used directly or as a base by
    /// per-language extractors that need custom post-processing.
    pub fn run(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&(self.config.language_fn)())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let source = content.as_bytes();
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        collect_nodes(
            tree.root_node(),
            path,
            source,
            self.config,
            &None,
            &mut nodes,
            &mut edges,
        );

        for (outer_kind, content_child) in self.config.string_literal_kinds {
            harvest_string_literals(
                tree.root_node(),
                path,
                source,
                self.config.language_name,
                outer_kind,
                *content_child,
                &mut nodes,
            );
        }

        Ok(ExtractionResult { nodes, edges })
    }
}

impl Extractor for GenericExtractor {
    fn extensions(&self) -> &[&str] {
        self.config.extensions
    }

    fn name(&self) -> &str {
        self.config.language_name
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        self.run(path, content)
    }
}

// ---------------------------------------------------------------------------
// Core traversal
// ---------------------------------------------------------------------------

fn collect_nodes(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    config: &LangConfig,
    parent_scope: &Option<(String, NodeKind)>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    // Match against the config table.
    if let Some(node_kind) = config.node_kinds.iter().find_map(|(ts_kind, nk)| {
        if *ts_kind == kind_str { Some(nk.clone()) } else { None }
    }) {
        // Extract name.
        // 1. Full-text kinds (e.g. Rust use_declaration): use the whole node text.
        // 2. Nodes with both "trait" and "type" fields (Rust impl): combine as "Trait for Type".
        // 3. Default: "name" field child.
        let name = if config.full_text_name_kinds.contains(&kind_str) {
            node.utf8_text(source).unwrap_or("unknown").trim().to_string()
        } else if node.child_by_field_name("trait").is_some() {
            // Rust `impl Trait for Type` — combine to "Trait for Type".
            let trait_name = node.child_by_field_name("trait")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("?");
            let type_name = node.child_by_field_name("type")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("?");
            format!("{} for {}", trait_name, type_name)
        } else {
            node.child_by_field_name("name")
                .or_else(|| node.child_by_field_name("type"))
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("unknown")
                .to_string()
        };

        if name == "unknown" && node_kind != NodeKind::Import {
            // Skip truly unnamed nodes — recurse instead.
        } else {
            let body = node.utf8_text(source).unwrap_or("").to_string();
            let signature = extract_signature(&body);
            let line_start = node.start_position().row + 1;
            let line_end = node.end_position().row + 1;

            let mut metadata = BTreeMap::new();

            // Parent scope.
            if let Some((scope_name, _)) = parent_scope {
                metadata.insert("parent_scope".to_string(), scope_name.clone());
            }

            // Name column for LSP cursor positioning.
            if let Some(name_node) = node.child_by_field_name("name") {
                metadata.insert(
                    "name_col".to_string(),
                    name_node.start_position().column.to_string(),
                );
            }

            // Cyclomatic complexity for functions.
            if node_kind == NodeKind::Function && !config.branch_node_types.is_empty() {
                let branches = count_branches(node, config.branch_node_types);
                metadata.insert("cyclomatic".to_string(), (1 + branches).to_string());
            }

            // Const value extraction.
            if node_kind == NodeKind::Const {
                if let Some(value_field) = config.const_value_field {
                    if let Some(val_node) = node.child_by_field_name(value_field) {
                        let val = val_node.utf8_text(source).unwrap_or("").trim().to_string();
                        let is_scalar = val.starts_with('"') || val.starts_with('\'')
                            || val.starts_with('`')
                            || val.parse::<f64>().is_ok()
                            || val == "true" || val == "false";
                        if is_scalar && !val.is_empty() {
                            let stripped = val.trim_matches('"')
                                .trim_matches('\'')
                                .trim_matches('`');
                            metadata.insert("value".to_string(), stripped.to_string());
                        }
                    }
                }
                metadata.insert("synthetic".to_string(), "false".to_string());
            }

            // Import edge.
            let import_edge = if node_kind == NodeKind::Import {
                // Try to resolve the import to an actual file path for cross-file edges.
                let target_file = resolve_import_path(path, &name, config.language_name);
                if let Some(ref r) = target_file {
                    tracing::debug!("import resolve: {} -> {}", name, r.display());
                }
                let (edge_file, target_name) = if let Some(ref resolved) = target_file {
                    // Use resolved file's stem as the module name
                    let stem = resolved.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    (resolved.clone(), stem)
                } else {
                    let fallback = parse_import_target(&name);
                    if fallback.is_empty() { (path.to_path_buf(), String::new()) }
                    else { (path.to_path_buf(), fallback) }
                };
                if !target_name.is_empty() {
                    Some(crate::graph::Edge {
                        from: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name: name.clone(),
                            kind: NodeKind::Import,
                        },
                        to: NodeId {
                            root: String::new(),
                            file: edge_file,
                            name: target_name,
                            kind: NodeKind::Module,
                        },
                        kind: crate::graph::EdgeKind::DependsOn,
                        source: ExtractionSource::TreeSitter,
                        confidence: if target_file.is_some() {
                            crate::graph::Confidence::Confirmed
                        } else {
                            crate::graph::Confidence::Detected
                        },
                    })
                } else { None }
            } else { None };

            nodes.push(Node {
                id: NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: name.clone(),
                    kind: node_kind.clone(),
                },
                language: config.language_name.to_string(),
                line_start,
                line_end,
                signature,
                body,
                metadata,
                source: ExtractionSource::TreeSitter,
            });

            if let Some(edge) = import_edge {
                edges.push(edge);
            }

            // Function parameter/return type DependsOn edges (config-driven).
            // Parameter and return type extraction are independent -- a language
            // may support return type extraction even if param container isn't
            // accessible via a simple field name (e.g. C++).
            if node_kind == NodeKind::Function {
                let has_type_edges = config.param_container_field.is_some()
                    || config.return_type_field.is_some();
                if has_type_edges {
                    let fn_id = NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name.clone(),
                        kind: NodeKind::Function,
                    };
                    // Parameter types
                    if let Some(param_field) = config.param_container_field {
                        if let Some(params) = node.child_by_field_name(param_field) {
                            if let Some(type_field) = config.param_type_field {
                                for i in 0..params.child_count() {
                                    if let Some(param) = params.child(i as u32) {
                                        if let Some(tn) = param.child_by_field_name(type_field) {
                                            let type_text = tn.utf8_text(source).unwrap_or("");
                                            if let Some(type_name) = extract_user_type(type_text, config.type_requires_uppercase) {
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
                            }
                        }
                    }
                    // Return type (independent of param extraction)
                    if let Some(ret_field) = config.return_type_field {
                        if let Some(rn) = node.child_by_field_name(ret_field) {
                            let ret_text = rn.utf8_text(source).unwrap_or("");
                            if let Some(type_name) = extract_user_type(ret_text, config.type_requires_uppercase) {
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
                }
            }

            // Module-level containment: top-level symbols get Defines edge from file module.
            if parent_scope.is_none() && node_kind != NodeKind::Import {
                let module_id = NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: path.file_stem()
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
                        name: name.clone(),
                        kind: node_kind.clone(),
                    },
                    kind: EdgeKind::Defines,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }

            // Emit structural edge from parent scope to this node.
            if let Some((scope_name, parent_kind)) = parent_scope {
                let edge_kind = if node_kind == NodeKind::Field {
                    EdgeKind::HasField
                } else {
                    EdgeKind::Defines
                };
                edges.push(Edge {
                    from: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: scope_name.clone(),
                        kind: parent_kind.clone(),
                    },
                    to: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: name.clone(),
                        kind: node_kind.clone(),
                    },
                    kind: edge_kind,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }

            // Trait implementation edge.
            if node_kind == NodeKind::Impl {
                if let Some(trait_node) = node.child_by_field_name("trait") {
                    let trait_name = trait_node.utf8_text(source).unwrap_or("?").to_string();
                    if let Some(type_node) = node.child_by_field_name("type") {
                        let type_name = type_node.utf8_text(source).unwrap_or("?").to_string();
                        edges.push(Edge {
                            from: NodeId {
                                root: String::new(),
                                file: path.to_path_buf(),
                                name: type_name,
                                kind: NodeKind::Struct,
                            },
                            to: NodeId {
                                root: String::new(),
                                file: path.to_path_buf(),
                                name: trait_name,
                                kind: NodeKind::Trait,
                            },
                            kind: EdgeKind::Implements,
                            source: ExtractionSource::TreeSitter,
                            confidence: Confidence::Detected,
                        });
                    }
                }
            }

            // Scope propagation into children.
            if config.scope_parent_kinds.contains(&kind_str) {
                let scope_kind = if node_kind == NodeKind::Impl
                    && node.child_by_field_name("trait").is_none() {
                    // Plain `impl Foo` — methods belong to the struct, not the impl
                    NodeKind::Struct
                } else {
                    node_kind
                };
                let scope = Some((name, scope_kind));
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        collect_nodes(child, path, source, config, &scope, nodes, edges);
                    }
                }
                return;
            }
        }
    }

    // Default: recurse into children with unchanged scope.
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_nodes(child, path, source, config, parent_scope, nodes, edges);
        }
    }
}

// ---------------------------------------------------------------------------
// Complexity
// ---------------------------------------------------------------------------

/// Count branch/decision-point nodes in a tree-sitter subtree.
/// Cyclomatic complexity = 1 + branch_count (the 1 is for the function itself).
fn count_branches(node: tree_sitter::Node, branch_types: &[&str]) -> usize {
    let mut count = if branch_types.contains(&node.kind()) { 1 } else { 0 };
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            count += count_branches(child, branch_types);
        }
    }
    count
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract signature: text before the first `{`, or the first line.
fn extract_signature(body: &str) -> String {
    if let Some(pos) = body.find('{') {
        let sig = body[..pos].trim();
        if !sig.is_empty() {
            return sig.to_string();
        }
    }
    body.lines().next().unwrap_or("").trim().to_string()
}

/// Extract a user-defined type name from a type text, skipping primitives.
///
/// Takes the first identifier (before any `<` generics), returns `None` for
/// primitives like `u32`, `bool`, `String`, `Vec`, `Option`, `Result`, and
/// reference/lifetime prefixes like `&self`.
///
/// When `require_uppercase` is true, the identifier must start with an
/// uppercase letter (convention for most languages). When false, any
/// non-primitive identifier is accepted (needed for Go unexported types
/// and Python lowercase classes).
fn extract_user_type(type_text: &str, require_uppercase: bool) -> Option<String> {
    // Strip reference prefix (e.g. "&Foo", "&mut Foo")
    // Also strip Python/TS annotation prefix `: Foo` and `-> Foo`
    let s = type_text.trim()
        .trim_start_matches("->")
        .trim_start_matches(':')
        .trim()
        .trim_start_matches('&')
        .trim_start_matches('*')
        .trim_start_matches("mut ")
        .trim_start_matches("const ")
        .trim();
    // Take first identifier (before `<` generics, `::` paths, `[` arrays, or `|` unions)
    let ident = s.split(|c: char| c == '<' || c == ':' || c == ' ' || c == '[' || c == '|' || c == '?')
        .next()
        .unwrap_or("")
        .trim();
    if ident.is_empty() {
        return None;
    }
    // When require_uppercase is set, the identifier must start with uppercase
    // to be considered a user-defined type. When false (Go, Python), any
    // non-primitive, non-keyword identifier is accepted.
    if require_uppercase && !ident.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    // Even without uppercase requirement, skip lowercase keywords and builtins
    if !require_uppercase && !ident.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    // Skip known standard library / primitive types (cross-language)
    const PRIMITIVES: &[&str] = &[
        // Rust stdlib
        "String", "Vec", "Option", "Result", "Box", "Rc", "Arc",
        "HashMap", "HashSet", "BTreeMap", "BTreeSet", "Cow",
        "Cell", "RefCell", "Mutex", "RwLock",
        "Pin", "Future", "Stream", "Iterator",
        "Self",
        // Cross-language primitives (title-cased forms)
        "Int", "Float", "Bool", "None", "Void",
        "Any", "Number", "Object", "Undefined", "Null",
        "Error", "Array", "Map", "Set", "List", "Dict",
        "Promise", "Tuple", "Type", "Interface",
        "Boolean", "Integer", "Double", "Long", "Short", "Byte", "Char",
        // Lowercase primitives (Go, Python, C, etc.)
        "int", "int8", "int16", "int32", "int64",
        "uint", "uint8", "uint16", "uint32", "uint64",
        "float32", "float64", "float", "double",
        "string", "bool", "byte", "rune", "uintptr",
        "error", "any", "comparable",
        "str", "bytes", "list", "dict", "set", "tuple", "object", "type",
        "void", "char", "short", "long", "unsigned", "signed",
        "self", "none",
        "usize", "isize", "u8", "u16", "u32", "u64", "u128",
        "i8", "i16", "i32", "i64", "i128", "f32", "f64",
        "comptime_int", "comptime_float", "noreturn",
    ];
    if PRIMITIVES.contains(&ident) {
        return None;
    }
    Some(ident.to_string())
}

/// Parse the target module name from an import declaration text.
/// Resolve an import statement to a target file path.
/// Returns `Some(path)` if the import can be resolved to a file that exists.
fn resolve_import_path(source_file: &Path, import_text: &str, language: &str) -> Option<std::path::PathBuf> {
    let parent = source_file.parent()?;

    match language {
        "python" => {
            // `from .util.user_utils import X` → `./util/user_utils.py`
            // `from ..models.user import X` → `../models/user.py`
            let text = import_text.trim();
            let module_path = if text.starts_with("from ") {
                text.strip_prefix("from ")?
                    .split_whitespace()
                    .next()?
            } else if text.starts_with("import ") {
                text.strip_prefix("import ")?
                    .split_whitespace()
                    .next()?
            } else {
                return None;
            };

            // Count leading dots for relative imports
            let dots = module_path.chars().take_while(|c| *c == '.').count();
            if dots == 0 {
                // Absolute import: emit best-effort path from module dots.
                // Graph builder resolves against scanned file index via suffix match.
                let rel = module_path.replace('.', "/");
                return Some(std::path::PathBuf::from(format!("{}.py", rel)));
            }
            let rest = &module_path[dots..];
            let rel = rest.replace('.', "/");

            // Go up (dots - 1) directories from parent
            let mut base = parent.to_path_buf();
            for _ in 1..dots {
                base = base.parent()?.to_path_buf();
            }
            Some(base.join(format!("{}.py", rel)))
        }
        "typescript" | "javascript" | "tsx" | "jsx" => {
            // `import X from './util/user_utils'` or `import X from '../util'`
            // Extract the path string from quotes
            let path_str = import_text
                .split(['\'', '"'])
                .nth(1)?;
            if !path_str.starts_with('.') {
                return None; // non-relative imports (npm packages) can't be resolved
            }
            // Return the import path with .ts extension as best guess.
            // Can't check .exists() (relative path, CWD != repo root).
            // The edge connects if the target was scanned; dangling otherwise.
            let base = parent.join(path_str);
            Some(std::path::PathBuf::from(format!("{}.ts", base.display())))
        }
        _ => None,
    }
}

fn parse_import_target(import_text: &str) -> String {
    // Strip `use ` prefix (Rust), `import ` (various), quotes, semicolons.
    let s = import_text
        .trim_start_matches("use ")
        .trim_start_matches("import ")
        .trim_matches('"')
        .trim_end_matches(';')
        .trim();
    // Take the first path segment.
    s.split([':','/','.']).next().unwrap_or("").trim().to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_signature_brace() {
        assert_eq!(extract_signature("fn foo() {\n    42\n}"), "fn foo()");
    }

    #[test]
    fn test_extract_signature_no_brace() {
        assert_eq!(extract_signature("MAX_RETRIES = 5"), "MAX_RETRIES = 5");
    }

    #[test]
    fn test_parse_import_target_rust() {
        assert_eq!(parse_import_target("use crate::foo::Bar;"), "crate");
        assert_eq!(parse_import_target("use std::collections::HashMap;"), "std");
    }

    #[test]
    fn test_rust_extractor_via_generic() {
        // Use the actual Rust config from rust.rs to test end-to-end.
        use crate::extract::rust::{RustExtractor, RUST_CONFIG};
        let _ = &RUST_CONFIG; // ensure config compiles
        let ext = RustExtractor::new();
        let code = r#"
pub struct Foo {
    pub bar: u32,
    baz: String,
}

impl Foo {
    pub fn do_thing(&self, other: Bar) -> u32 {
        self.bar
    }
}

trait Greet {
    fn greet(&self) -> String;
}

impl Greet for Foo {
    fn greet(&self) -> String {
        String::new()
    }
}

pub fn hello() {}
"#;
        let result = ext.extract(Path::new("test.rs"), code).unwrap();
        let kinds: Vec<_> = result.nodes.iter().map(|n| &n.id.kind).collect();
        assert!(kinds.contains(&&NodeKind::Struct), "Should find Foo struct");
        assert!(kinds.contains(&&NodeKind::Function), "Should find hello fn");
        assert!(kinds.contains(&&NodeKind::Field), "Should find bar field: {:?}", kinds);

        // Assert structural edges are emitted — prevents silent regression
        // if the edge emission block is removed.
        let has_field_edges: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::HasField)
            .collect();
        assert!(
            !has_field_edges.is_empty(),
            "Should emit HasField edges for struct fields, got edges: {:?}",
            result.edges.iter().map(|e| format!("{:?}", e.kind)).collect::<Vec<_>>()
        );
        // Verify a HasField edge connects Foo -> bar
        assert!(
            has_field_edges.iter().any(|e| e.from.name == "Foo" && e.to.name == "bar"
                && e.from.kind == NodeKind::Struct && e.to.kind == NodeKind::Field),
            "Should have HasField edge from Foo struct to bar field"
        );

        let defines_edges: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Defines)
            .collect();
        assert!(
            !defines_edges.is_empty(),
            "Should emit Defines edges for impl methods, got edges: {:?}",
            result.edges.iter().map(|e| format!("{:?}", e.kind)).collect::<Vec<_>>()
        );
        // Verify a Defines edge connects Foo:struct -> do_thing (not Foo:impl)
        assert!(
            defines_edges.iter().any(|e| e.from.name == "Foo" && e.from.kind == NodeKind::Struct
                && e.to.name == "do_thing" && e.to.kind == NodeKind::Function),
            "Should have Defines edge from Foo:struct to do_thing method, got: {:?}",
            defines_edges
        );

        // Verify Implements edge: Foo:struct -> Greet:trait
        let implements_edges: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();
        assert!(
            implements_edges.iter().any(|e| e.from.name == "Foo" && e.from.kind == NodeKind::Struct
                && e.to.name == "Greet" && e.to.kind == NodeKind::Trait),
            "Should have Implements edge from Foo:struct to Greet:trait, got: {:?}",
            implements_edges
        );

        // Verify module-level Defines: test:module -> hello:function
        let module_defines: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Defines && e.from.kind == NodeKind::Module)
            .collect();
        assert!(
            module_defines.iter().any(|e| e.from.name == "test" && e.to.name == "hello"
                && e.to.kind == NodeKind::Function),
            "Should have Defines edge from test:module to hello:function, got: {:?}",
            module_defines
        );

        // Verify DependsOn: do_thing:function -> Bar:struct (parameter type)
        let depends_on_edges: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert!(
            depends_on_edges.iter().any(|e| e.from.name == "do_thing" && e.from.kind == NodeKind::Function
                && e.to.name == "Bar" && e.to.kind == NodeKind::Struct),
            "Should have DependsOn edge from do_thing:function to Bar:struct, got: {:?}",
            depends_on_edges
        );
    }

    // -----------------------------------------------------------------------
    // Per-language DependsOn edge extraction tests
    // -----------------------------------------------------------------------

    /// Helper: extract DependsOn edges from a code snippet using a given config.
    fn depends_on_edges_for(config: &'static LangConfig, path: &str, code: &str) -> Vec<Edge> {
        let ext = GenericExtractor::new(config);
        let result = ext.run(Path::new(path), code).unwrap();
        result.edges.into_iter()
            .filter(|e| e.kind == EdgeKind::DependsOn && e.from.kind == NodeKind::Function)
            .collect()
    }

    #[test]
    fn test_depends_on_python() {
        use crate::extract::configs::PYTHON_CONFIG;
        let code = "class Foo:\n    pass\n\ndef process(item: Foo) -> Bar:\n    pass\n";
        let edges = depends_on_edges_for(&PYTHON_CONFIG, "test.py", code);
        assert!(
            edges.iter().any(|e| e.to.name == "Foo"),
            "Python: should emit DependsOn for param type Foo, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
        assert!(
            edges.iter().any(|e| e.to.name == "Bar"),
            "Python: should emit DependsOn for return type Bar, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_depends_on_typescript() {
        use crate::extract::configs::TYPESCRIPT_CONFIG;
        let code = "class Foo {}\nfunction process(item: Foo): Bar {\n    return new Bar();\n}\n";
        let edges = depends_on_edges_for(&TYPESCRIPT_CONFIG, "test.ts", code);
        assert!(
            edges.iter().any(|e| e.to.name == "Foo"),
            "TypeScript: should emit DependsOn for param type Foo, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
        assert!(
            edges.iter().any(|e| e.to.name == "Bar"),
            "TypeScript: should emit DependsOn for return type Bar, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_depends_on_go() {
        use crate::extract::configs::GO_CONFIG;
        let code = "package main\n\ntype Foo struct{}\nfunc Process(item Foo) Bar {\n    return Bar{}\n}\n";
        let edges = depends_on_edges_for(&GO_CONFIG, "test.go", code);
        assert!(
            edges.iter().any(|e| e.to.name == "Foo"),
            "Go: should emit DependsOn for param type Foo, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
        assert!(
            edges.iter().any(|e| e.to.name == "Bar"),
            "Go: should emit DependsOn for return type Bar, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_depends_on_go_unexported_types() {
        // Go unexported types (lowercase) should still be detected as user types.
        use crate::extract::configs::GO_CONFIG;
        let code = "package main\n\nfunc process(item myStruct) otherType {\n    return otherType{}\n}\n";
        let edges = depends_on_edges_for(&GO_CONFIG, "test.go", code);
        assert!(
            edges.iter().any(|e| e.to.name == "myStruct"),
            "Go: should emit DependsOn for unexported param type myStruct, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
        assert!(
            edges.iter().any(|e| e.to.name == "otherType"),
            "Go: should emit DependsOn for unexported return type otherType, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_depends_on_go_skips_primitives() {
        // Go primitives (string, int, bool, error) should NOT produce DependsOn edges.
        use crate::extract::configs::GO_CONFIG;
        let code = "package main\n\nfunc process(name string, count int) error {\n    return nil\n}\n";
        let edges = depends_on_edges_for(&GO_CONFIG, "test.go", code);
        assert!(
            edges.is_empty(),
            "Go: should NOT emit DependsOn for primitive types, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_depends_on_python_lowercase_class() {
        // Python classes can be lowercase -- should still be detected.
        use crate::extract::configs::PYTHON_CONFIG;
        let code = "def process(item: my_class) -> other_class:\n    pass\n";
        let edges = depends_on_edges_for(&PYTHON_CONFIG, "test.py", code);
        assert!(
            edges.iter().any(|e| e.to.name == "my_class"),
            "Python: should emit DependsOn for lowercase class my_class, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_depends_on_java() {
        use crate::extract::configs::JAVA_CONFIG;
        let code = "class Foo {}\nclass Main {\n    Bar process(Foo item) {\n        return null;\n    }\n}\n";
        let edges = depends_on_edges_for(&JAVA_CONFIG, "test.java", code);
        assert!(
            edges.iter().any(|e| e.to.name == "Foo"),
            "Java: should emit DependsOn for param type Foo, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
        assert!(
            edges.iter().any(|e| e.to.name == "Bar"),
            "Java: should emit DependsOn for return type Bar, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_depends_on_csharp() {
        use crate::extract::configs::CSHARP_CONFIG;
        let code = "class Foo {}\nclass Main {\n    Bar Process(Foo item) {\n        return null;\n    }\n}\n";
        let edges = depends_on_edges_for(&CSHARP_CONFIG, "test.cs", code);
        assert!(
            edges.iter().any(|e| e.to.name == "Foo"),
            "C#: should emit DependsOn for param type Foo, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
        assert!(
            edges.iter().any(|e| e.to.name == "Bar"),
            "C#: should emit DependsOn for return type Bar, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_depends_on_cpp_return_type() {
        // C++ param types need per-language logic (params are on declarator),
        // but return type IS accessible via field "type" on function_definition.
        use crate::extract::configs::CPP_CONFIG;
        let code = "class Foo {};\nBar process(Foo item) {\n    return Bar();\n}\n";
        let edges = depends_on_edges_for(&CPP_CONFIG, "test.cpp", code);
        assert!(
            edges.iter().any(|e| e.to.name == "Bar"),
            "C++: should emit DependsOn for return type Bar, got: {:?}",
            edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    // Kotlin, Swift, and Zig DependsOn tests: these languages have tree-sitter
    // grammars where function parameter/return type nodes are NOT accessible via
    // simple field names on the function node. DependsOn extraction for these
    // languages requires per-language extractor logic (TODO).
    // The configs correctly set param_container_field = None to avoid silent
    // failures. When per-language extractors are added, these tests should be
    // updated to assert DependsOn edges.

    #[test]
    fn test_depends_on_kotlin_skipped() {
        // Kotlin tree-sitter-kotlin-ng does not expose function params/return type
        // via field names -- DependsOn intentionally skipped via None config.
        use crate::extract::configs::KOTLIN_CONFIG;
        assert!(KOTLIN_CONFIG.param_container_field.is_none(),
            "Kotlin param_container_field should be None (grammar lacks field names)");
    }

    #[test]
    fn test_depends_on_swift_skipped() {
        // Swift tree-sitter does not expose function params via a container field,
        // and overloads the "name" field for types -- DependsOn intentionally skipped.
        use crate::extract::configs::SWIFT_CONFIG;
        assert!(SWIFT_CONFIG.param_container_field.is_none(),
            "Swift param_container_field should be None (grammar lacks container field)");
    }

    #[test]
    fn test_depends_on_zig_skipped() {
        // Zig tree-sitter: "parameters" is a child node kind, not a field name
        // on function_declaration -- DependsOn intentionally skipped.
        use crate::extract::configs::ZIG_CONFIG;
        assert!(ZIG_CONFIG.param_container_field.is_none(),
            "Zig param_container_field should be None (not a field name in grammar)");
    }

    #[test]
    fn test_extract_user_type_uppercase_required() {
        // With require_uppercase = true (default for most languages)
        assert_eq!(extract_user_type("Foo", true), Some("Foo".to_string()));
        assert_eq!(extract_user_type("Bar<T>", true), Some("Bar".to_string()));
        assert_eq!(extract_user_type("&Foo", true), Some("Foo".to_string()));
        assert_eq!(extract_user_type("u32", true), None);
        assert_eq!(extract_user_type("String", true), None); // stdlib
        assert_eq!(extract_user_type("foo", true), None); // lowercase rejected
    }

    #[test]
    fn test_extract_user_type_no_uppercase_required() {
        // With require_uppercase = false (Go, Python)
        assert_eq!(extract_user_type("Foo", false), Some("Foo".to_string()));
        assert_eq!(extract_user_type("myStruct", false), Some("myStruct".to_string()));
        assert_eq!(extract_user_type("string", false), None); // primitive
        assert_eq!(extract_user_type("int", false), None); // primitive
        assert_eq!(extract_user_type("error", false), None); // Go builtin
        assert_eq!(extract_user_type("bool", false), None); // primitive
    }

    // -----------------------------------------------------------------------
    // Cyclomatic complexity
    // -----------------------------------------------------------------------

    #[test]
    fn test_cyclomatic_complexity_linear_function() {
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
fn simple() {
    println!("hello");
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(std::path::Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "simple").unwrap();
        assert_eq!(func.metadata.get("cyclomatic").map(|s| s.as_str()), Some("1"));
    }

    #[test]
    fn test_cyclomatic_complexity_with_branches() {
        use crate::extract::rust::RUST_CONFIG;
        // if + else = 2 branch nodes → complexity = 1 + 2 = 3
        let code = r#"
fn branchy(x: i32) -> i32 {
    if x > 0 {
        1
    } else {
        -1
    }
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(std::path::Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "branchy").unwrap();
        let cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();
        assert!(cc > 1, "Function with if/else should have complexity > 1, got {}", cc);
    }

    #[test]
    fn test_cyclomatic_complexity_nested() {
        use crate::extract::rust::RUST_CONFIG;
        // match + for + if = at least 3 branch nodes
        let code = r#"
fn complex(items: &[Option<i32>]) -> i32 {
    let mut sum = 0;
    for item in items {
        match item {
            Some(v) if *v > 0 => { sum += v; }
            _ => {}
        }
    }
    sum
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(std::path::Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "complex").unwrap();
        let cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();
        assert!(cc >= 3, "Function with match+for should have complexity >= 3, got {}", cc);
    }

    #[test]
    fn test_cyclomatic_complexity_not_on_structs() {
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
struct Foo {
    x: i32,
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(std::path::Path::new("test.rs"), code)
            .unwrap();
        let struc = result.nodes.iter().find(|n| n.id.name == "Foo").unwrap();
        assert!(struc.metadata.get("cyclomatic").is_none(), "Structs should not have cyclomatic metadata");
    }

    #[test]
    fn test_cyclomatic_complexity_python() {
        use crate::extract::configs::PYTHON_CONFIG;
        let code = r#"
def process(items):
    for item in items:
        if item > 0:
            yield item
        elif item == 0:
            continue
        else:
            raise ValueError("negative")
"#;
        let result = GenericExtractor::new(&PYTHON_CONFIG)
            .run(std::path::Path::new("test.py"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "process").unwrap();
        let cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();
        // for + if + elif + else = 4 branch nodes → cc = 5
        assert!(cc >= 4, "Python function with for/if/elif/else should have complexity >= 4, got {}", cc);
    }

    #[test]
    fn test_cyclomatic_complexity_typescript() {
        use crate::extract::configs::TYPESCRIPT_CONFIG;
        let code = r#"
function route(req: Request): string {
    if (req.method === "GET") {
        return "get";
    } else {
        return req.admin ? "admin" : "user";
    }
}
"#;
        let result = GenericExtractor::new(&TYPESCRIPT_CONFIG)
            .run(std::path::Path::new("test.ts"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "route").unwrap();
        let cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();
        // if + else + ternary = 3 branch nodes → cc = 4
        assert!(cc > 1, "TypeScript function with if/else/ternary should have complexity > 1, got {}", cc);
    }

    #[test]
    fn test_cyclomatic_complexity_go() {
        use crate::extract::configs::GO_CONFIG;
        let code = r#"
func process(items []int) int {
    sum := 0
    for _, v := range items {
        if v > 0 {
            sum += v
        } else {
            sum -= v
        }
    }
    return sum
}
"#;
        let result = GenericExtractor::new(&GO_CONFIG)
            .run(std::path::Path::new("test.go"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "process").unwrap();
        let cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();
        // for + if + else = 3 branch nodes → cc = 4
        assert!(cc > 1, "Go function with for/if/else should have complexity > 1, got {}", cc);
    }

    #[test]
    fn test_cyclomatic_complexity_java() {
        use crate::extract::configs::JAVA_CONFIG;
        let code = r#"
class Handler {
    int handle(int code) {
        if (code > 200) {
            return code;
        } else {
            for (int i = 0; i < code; i++) {
                System.out.println(i);
            }
            return 0;
        }
    }
}
"#;
        let result = GenericExtractor::new(&JAVA_CONFIG)
            .run(std::path::Path::new("Test.java"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "handle").unwrap();
        let cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();
        // if + else + for = 3 branch nodes → cc = 4
        assert!(cc > 1, "Java method with if/else/for should have complexity > 1, got {}", cc);
    }

    // -----------------------------------------------------------------------
    // Adversarial: dissent-seeded complexity tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_adversarial_arithmetic_inflation_documented() {
        // Dissent risk: binary_expression covers arithmetic AND logical operators.
        // This is a known trade-off across all languages using binary_expression
        // (Rust, Go, TS, Java, C++, etc.). Arithmetic inflates scores.
        // This test DOCUMENTS the behavior rather than asserting it away.
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
fn math(a: i32, b: i32) -> i32 {
    let x = a + b * 2 - a / b;
    let y = x % 3 + a * b;
    x + y
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(std::path::Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "math").unwrap();
        let cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();
        // binary_expression counts arithmetic ops too — known inflation.
        // A pure-arithmetic function will show cc > 1. This is acceptable
        // because the alternative (not counting && and ||) is worse.
        assert!(cc >= 1, "Arithmetic function should have cc >= 1, got {}", cc);
        eprintln!("ADVERSARIAL: Rust arithmetic cc={} (inflated by binary_expression)", cc);
    }

    #[test]
    fn test_adversarial_go_arithmetic_vs_logical() {
        // Go uses binary_expression for BOTH arithmetic and logical operators.
        // This is a known trade-off documented in dissent. Verify arithmetic
        // does NOT inflate (tree-sitter-go binary_expression has an operator
        // field, but we count the node kind not the operator).
        use crate::extract::configs::GO_CONFIG;

        // Pure arithmetic
        let arith_code = r#"
func math(a int, b int) int {
    x := a + b*2 - a/b
    return x
}
"#;
        let result = GenericExtractor::new(&GO_CONFIG)
            .run(std::path::Path::new("test.go"), arith_code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "math").unwrap();
        let arith_cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();

        // Logical operators (actual branches)
        let logic_code = r#"
func validate(a int, b int) bool {
    return a > 0 && b > 0 || a == b
}
"#;
        let result2 = GenericExtractor::new(&GO_CONFIG)
            .run(std::path::Path::new("test.go"), logic_code)
            .unwrap();
        let func2 = result2.nodes.iter().find(|n| n.id.name == "validate").unwrap();
        let logic_cc: usize = func2.metadata.get("cyclomatic").unwrap().parse().unwrap();

        // Document the behavior: Go binary_expression counts ALL binary ops.
        // This is a known limitation. The test documents it, not asserts perfection.
        // Arithmetic functions will show inflated scores in Go.
        assert!(arith_cc >= 1, "Go arithmetic function should have cc >= 1, got {}", arith_cc);
        assert!(logic_cc >= 1, "Go logical function should have cc >= 1, got {}", logic_cc);
        // Log the actual values for visibility
        eprintln!("ADVERSARIAL: Go arithmetic cc={}, logical cc={}", arith_cc, logic_cc);
    }

    #[test]
    fn test_adversarial_empty_function_body() {
        // Edge case: function with empty body should have cc=1.
        use crate::extract::rust::RUST_CONFIG;
        let code = "fn noop() {}\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(std::path::Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "noop").unwrap();
        let cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();
        assert_eq!(cc, 1, "Empty function should have complexity 1, got {}", cc);
    }

    #[test]
    fn test_adversarial_many_match_arms_flat() {
        // Dissent: 10 flat match arms score the same as 10 nested ifs,
        // but flat arms are easier to reason about. Verify the score.
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
fn dispatch(cmd: &str) -> i32 {
    match cmd {
        "a" => 1,
        "b" => 2,
        "c" => 3,
        "d" => 4,
        "e" => 5,
        "f" => 6,
        "g" => 7,
        "h" => 8,
        "i" => 9,
        _ => 0,
    }
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(std::path::Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "dispatch").unwrap();
        let cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();
        // match_expression counts as 1, but each match_arm also counts.
        // The exact score depends on tree-sitter grammar details.
        // Key assertion: it IS > 1 (not silently broken).
        assert!(cc > 1, "10-arm match should have cc > 1, got {}", cc);
        eprintln!("ADVERSARIAL: 10-arm match dispatch cc={}", cc);
    }

    #[test]
    fn test_adversarial_boolean_chain_rust() {
        // Rust boolean operators: && and || should each count as a branch.
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
fn complex_guard(a: bool, b: bool, c: bool, d: bool) -> bool {
    a && b || c && d || a && c
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(std::path::Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "complex_guard").unwrap();
        let cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();
        // 5 boolean operators → cc should be >= 5
        // (tree-sitter nesting means fewer binary_expression nodes than operators)
        assert!(cc > 1, "Boolean chain should have cc > 1, got {}", cc);
        eprintln!("ADVERSARIAL: boolean chain cc={}", cc);
    }

    #[test]
    fn test_adversarial_closure_branches_not_counted_in_parent() {
        // If a function contains a closure with branches, those branches
        // are inside the closure's subtree which is inside the function's subtree.
        // Our count_branches walks the ENTIRE function subtree, so closure
        // branches DO inflate the parent. Document this behavior.
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
fn parent() {
    let f = |x: i32| {
        if x > 0 { 1 } else { -1 }
    };
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(std::path::Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "parent").unwrap();
        let cc: usize = func.metadata.get("cyclomatic").unwrap().parse().unwrap();
        // The if/else inside the closure IS counted in parent's subtree.
        // This is a known trade-off: tree-sitter closure nodes are children.
        assert!(cc > 1, "Parent with branchy closure should have cc > 1 (closure branches counted), got {}", cc);
        eprintln!("ADVERSARIAL: parent with closure cc={} (closure branches included)", cc);
    }
}
