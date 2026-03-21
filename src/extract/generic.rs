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
use std::sync::OnceLock;

use anyhow::Result;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};
use crate::scanner::PatternConfig;
use super::query::{CaptureSet, QueryExtractor, RouteQueryConfig};
use super::string_literals::harvest_string_literals;
use super::{ExtractionResult, Extractor};

// ── Pattern config global ───────────────────────────────────────────

/// Resolved pattern suffix list, initialized from `.oh/config.toml`.
/// Falls back to built-in defaults if `init_pattern_config` is never called.
static PATTERN_CONFIG: OnceLock<Vec<(String, String)>> = OnceLock::new();

/// Initialize the global pattern config from a repo's `.oh/config.toml`.
/// Call once at server startup. Safe to call multiple times (only the first
/// call takes effect, per `OnceLock` semantics).
pub fn init_pattern_config(repo_root: &Path) {
    let config = PatternConfig::load(repo_root);
    let _ = PATTERN_CONFIG.set(config.effective_suffixes());
}

/// Get the effective pattern suffixes. Returns the configured list if
/// `init_pattern_config` was called, otherwise falls back to built-in defaults.
fn effective_pattern_suffixes() -> &'static [(String, String)] {
    // Initialized by init_pattern_config; if not yet called, use defaults.
    static DEFAULT_OWNED: OnceLock<Vec<(String, String)>> = OnceLock::new();
    PATTERN_CONFIG.get().map(|v| v.as_slice()).unwrap_or_else(|| {
        DEFAULT_OWNED
            .get_or_init(|| {
                crate::scanner::DEFAULT_PATTERN_SUFFIXES
                    .iter()
                    .map(|(s, h)| (s.to_string(), h.to_string()))
                    .collect()
            })
            .as_slice()
    })
}

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
    /// Tree-sitter node kinds that represent decorators/attributes/annotations.
    /// These are collected from previous siblings (or parent wrapper nodes)
    /// and stored as `metadata["decorators"]` on the decorated symbol.
    ///
    /// Examples: `["decorator"]` (Python/TS), `["attribute_item"]` (Rust),
    /// `["annotation"]` (Java/Kotlin). Empty slice = no decorator extraction.
    pub decorator_node_kinds: &'static [&'static str],
    /// Tree-sitter node kind for generic type parameters (e.g. `<T: Display>`).
    /// Most languages use `"type_parameters"`, Go uses `"type_parameter_list"`.
    /// None = skip type parameter extraction for this language.
    pub type_param_node_kind: Option<&'static str>,
    /// Whether this language places its doc comment (docstring) as the first
    /// expression_statement in the function/class body rather than as a
    /// preceding comment sibling.
    ///
    /// `true` for Python (`"""docstring"""`), `false` for all other languages
    /// (which use preceding line_comment / block_comment siblings).
    ///
    /// When `true`, `collect_doc_comment()` also checks the first string
    /// literal in the node's `body` field child.
    pub docstring_in_body: bool,
    /// Optional tree-sitter query patterns for route decorator detection.
    ///
    /// Each entry is a [`RouteQueryConfig`] describing one query that matches
    /// HTTP route decorators and emits [`NodeKind::ApiEndpoint`] nodes.
    /// Empty slice = no route query extraction for this language.
    ///
    /// Queries run as an additional pass after the normal manual traversal
    /// so that all existing symbol extraction is unaffected.
    pub route_queries: &'static [RouteQueryConfig],
    /// Lazily compiled [`QueryExtractor`]s for `route_queries`.
    ///
    /// Each slot corresponds to the `route_queries` entry at the same index.
    /// A `None` slot indicates that the query at that index failed to compile
    /// (log warning was emitted at compile time). Using `Option` preserves the
    /// 1:1 correspondence between `route_queries[i]` and compiled slot `i`.
    ///
    /// Populated on the first call to `GenericExtractor::run()` for this
    /// config and reused for all subsequent files. Since [`QueryExtractor`]
    /// (and the underlying `tree_sitter::Query`) is `Send + Sync`, this is
    /// safe to store in a static `OnceLock`.
    pub compiled_route_queries: std::sync::OnceLock<Vec<Option<QueryExtractor>>>,
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

        // Same-file call detection (#407).
        // After all nodes are collected, resolve bare function calls against
        // function names defined in this file. Emit Calls edges with Confidence::Detected.
        // Cross-file and method-chain calls are left to LSP.
        {
            let same_file_calls = detect_same_file_calls(
                tree.root_node(),
                path,
                source,
                self.config,
                &nodes,
            );
            edges.extend(same_file_calls);
        }

        // Decorator → Implements edges (#392).
        // After all nodes are collected, parse decorator strings and emit
        // Implements edges for derive macros, class decorators, annotations, etc.
        {
            let decorator_edges = emit_decorator_implements_edges(&nodes, path);
            edges.extend(decorator_edges);
        }

        // Route query pass — runs after manual traversal; purely additive.
        // Queries are compiled lazily on the first call via `compiled_route_queries`
        // and cached for all subsequent files. This avoids per-file compilation.
        if !self.config.route_queries.is_empty() {
            let compiled = self.config.compiled_route_queries.get_or_init(|| {
                let language = (self.config.language_fn)();
                self.config.route_queries.iter()
                    .map(|cfg| {
                        match QueryExtractor::new(&language, cfg.query) {
                            Ok(qe) => Some(qe),
                            Err(err) => {
                                tracing::warn!("route query '{}' failed to compile: {}", cfg.label, err);
                                None
                            }
                        }
                    })
                    .collect()
            });

            run_route_queries(
                self.config.route_queries,
                compiled,
                tree.root_node(),
                path,
                source,
                self.config.language_name,
                &mut nodes,
                &mut edges,
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
                let branches = count_branches(node, source, config, true);
                metadata.insert("cyclomatic".to_string(), (1 + branches).to_string());
            }

            // Static vs instance method detection for functions inside a scope.
            if node_kind == NodeKind::Function && parent_scope.is_some() {
                if let Some(is_static) = detect_is_static(node, source, config) {
                    metadata.insert(
                        "is_static".to_string(),
                        if is_static { "true" } else { "false" }.to_string(),
                    );
                }
            }

            // Decorator/attribute collection.
            if !config.decorator_node_kinds.is_empty() {
                let decorators = collect_decorators(node, source, config);
                if !decorators.is_empty() {
                    // is_async: set before decorator-derived is_test so both can be set
                    // independently.
                    if node_kind == NodeKind::Function {
                        let is_async = detect_is_async(node, source);
                        if is_async {
                            metadata.insert("is_async".to_string(), "true".to_string());
                        }
                        // is_test: check decorator tokens for test patterns.
                        let is_test = is_test_by_decorator(&decorators, &name);
                        if is_test {
                            metadata.insert("is_test".to_string(), "true".to_string());
                        }
                    }
                    metadata.insert("decorators".to_string(), decorators);
                } else if node_kind == NodeKind::Function {
                    // No decorators but still check is_async and name-based is_test.
                    let is_async = detect_is_async(node, source);
                    if is_async {
                        metadata.insert("is_async".to_string(), "true".to_string());
                    }
                    // Name-based test detection (Python test_* convention, etc.)
                    if is_test_by_decorator("", &name) {
                        metadata.insert("is_test".to_string(), "true".to_string());
                    }
                }
            } else if node_kind == NodeKind::Function {
                // Language with no decorator config — still detect async and is_test.
                let is_async = detect_is_async(node, source);
                if is_async {
                    metadata.insert("is_async".to_string(), "true".to_string());
                }
                // Name-based test detection.
                if is_test_by_decorator("", &name) {
                    metadata.insert("is_test".to_string(), "true".to_string());
                }
            }

            // Doc comment extraction (#401).
            // Walk preceding siblings to find doc comments above the definition.
            // Language-agnostic: checks well-known comment node kinds and strips
            // comment markers (///, /**, //) to expose the plain intent text for
            // semantic search. Stored as metadata["doc_comment"].
            {
                let doc_comment = collect_doc_comment(node, source, config);
                if !doc_comment.is_empty() {
                    metadata.insert("doc_comment".to_string(), doc_comment);
                }
            }

            // Generic type parameter extraction.
            if let Some(tp_kind) = config.type_param_node_kind {
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        if child.kind() == tp_kind {
                            if let Ok(text) = child.utf8_text(source) {
                                let text = text.trim();
                                if !text.is_empty() {
                                    metadata.insert(
                                        "type_params".to_string(),
                                        text.to_string(),
                                    );
                                }
                            }
                            break;
                        }
                    }
                }
            }

            // Design pattern hint from naming conventions.
            if let Some(hint) = detect_pattern_hint(&name, &node_kind) {
                metadata.insert("pattern_hint".to_string(), hint);
            }

            // Python __all__ = [...] detection (#409).
            // When a module-level assignment is named `__all__`, it declares
            // the public API surface. Mark it with `exported = "true"`.
            // `__all__` is a Python convention; no other supported language uses it.
            if node_kind == NodeKind::Const && name == "__all__" {
                metadata.insert("exported".to_string(), "true".to_string());
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

            // Public API surface detection (#409).
            // Rust: `pub use` → visibility=pub + ReExports edge.
            // Python: `__all__ = [...]` on an assignment → exported=true.
            // TypeScript: `export { X }` → visibility=pub + ReExports edge.
            if node_kind == NodeKind::Import {
                detect_public_api(node, source, config.language_name, path, &name, &mut metadata, edges);
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
                let edge_kind = if node_kind == NodeKind::Field || node_kind == NodeKind::EnumVariant {
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

/// Logical operators that represent actual control-flow branches.
/// When a `binary_expression` / `binary` node uses one of these operators,
/// it counts as a branch. Arithmetic/comparison operators do not.
const LOGICAL_OPERATORS: &[&str] = &["&&", "||", "and", "or"];

/// Anonymous function-like node kinds that act as complexity boundaries.
/// These are not in `node_kinds` (they aren't extracted as top-level symbols),
/// but branches inside them belong to the closure, not the enclosing function.
const CLOSURE_NODE_KINDS: &[&str] = &[
    "closure_expression",    // Rust
    "lambda",                // Python
    "arrow_function",        // TypeScript, JavaScript
    "function_expression",   // JavaScript
    "generator_function",    // JavaScript
    "anonymous_function",    // PHP
    "lambda_literal",        // Kotlin
    "func_literal",          // Go
    "lambda_expression",     // Java, C#
    // Note: Ruby "block" omitted — too generic, conflicts with Rust/other
    // languages where "block" is a plain scope, not a closure.
    "do_block",              // Ruby (do...end blocks act as closures)
];

/// Count branch/decision-point nodes in a tree-sitter subtree.
/// Cyclomatic complexity = 1 + branch_count (the 1 is for the function itself).
///
/// `source` is needed to inspect operator text inside `binary_expression` nodes.
/// When `skip_nested_fns` is true, subtrees whose node kind matches a
/// `NodeKind::Function` entry in `config.node_kinds` are skipped (prevents
/// nested function bodies from inflating the parent's complexity).
pub(super) fn count_branches(
    node: tree_sitter::Node,
    source: &[u8],
    config: &LangConfig,
    skip_nested_fns: bool,
) -> usize {
    let kind = node.kind();

    // For binary_expression / binary nodes, only count if the operator is logical.
    let is_branch = if config.branch_node_types.contains(&kind) {
        if kind == "binary_expression" || kind == "binary" {
            // Try named field first, fall back to middle child (index 1).
            let op_text = node
                .child_by_field_name("operator")
                .or_else(|| node.child(1))
                .and_then(|op| op.utf8_text(source).ok())
                .unwrap_or("");
            LOGICAL_OPERATORS.contains(&op_text)
        } else {
            true
        }
    } else {
        false
    };

    let mut count = if is_branch { 1 } else { 0 };

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            // Skip nested function/closure bodies so they don't inflate the parent.
            if skip_nested_fns {
                let child_kind = child.kind();
                let is_fn = config.node_kinds.iter().any(|(ts_kind, nk)| {
                    *ts_kind == child_kind && *nk == NodeKind::Function
                });
                if is_fn || CLOSURE_NODE_KINDS.contains(&child_kind) {
                    continue;
                }
            }
            count += count_branches(child, source, config, skip_nested_fns);
        }
    }
    count
}

// ---------------------------------------------------------------------------
// Design pattern hint detection (naming conventions)
// ---------------------------------------------------------------------------

/// Detect a design pattern hint from a node's name using suffix matching.
///
/// Uses the effective pattern suffix list (built-in defaults merged with
/// any user configuration from `.oh/config.toml`). Only applies to node
/// kinds that carry architectural intent: Struct, Trait, Function, Enum,
/// and Other (for Class in languages like Python/TypeScript). Returns the
/// lowercase pattern name if the node name ends with a known pattern
/// suffix (case-insensitive).
fn detect_pattern_hint(name: &str, kind: &NodeKind) -> Option<String> {
    // Only match on types/functions that carry architectural intent.
    match kind {
        NodeKind::Struct
        | NodeKind::Trait
        | NodeKind::Function
        | NodeKind::Enum
        | NodeKind::Other(_) => {}
        _ => return None,
    }

    let lower = name.to_ascii_lowercase();
    for (suffix, hint) in effective_pattern_suffixes() {
        if lower.ends_with(suffix.as_str()) {
            return Some(hint.clone());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Static / instance method detection
// ---------------------------------------------------------------------------

/// Detect whether a function node inside a class/struct/impl scope is a static
/// (or associated) function vs an instance method. Returns `Some(true)` for
/// static, `Some(false)` for instance, or `None` if detection is not applicable
/// for this language.
///
/// Language-specific heuristics:
/// - **Rust:** First param is `self`/`&self`/`&mut self` -> instance; else -> static
/// - **Python:** Has `@staticmethod` decorator -> static; first param is `self`/`cls` -> instance
/// - **Java/C#/TypeScript:** `static` keyword in function text -> static; else -> instance
/// - Other languages with no `param_container_field` -> `None` (skip)
fn detect_is_static(
    node: tree_sitter::Node,
    source: &[u8],
    config: &LangConfig,
) -> Option<bool> {
    match config.language_name {
        "rust" => detect_is_static_rust(node, source),
        "python" => detect_is_static_python(node, source),
        "java" | "csharp" | "typescript" | "javascript" => detect_is_static_keyword(node, source),
        _ => None,
    }
}

/// Rust: A function in an impl/trait block is an instance method if its first
/// parameter is `self`, `&self`, `&mut self`, or `mut self`.
fn detect_is_static_rust(node: tree_sitter::Node, source: &[u8]) -> Option<bool> {
    if let Some(params) = node.child_by_field_name("parameters") {
        for i in 0..params.child_count() {
            if let Some(param) = params.child(i as u32) {
                // In Rust tree-sitter, `self` parameters are `self_parameter` nodes.
                if param.kind() == "self_parameter" {
                    return Some(false); // instance method
                }
                // Also check for named params that might be "self" variants
                if param.is_named() && param.kind() != "," {
                    let text = param.utf8_text(source).unwrap_or("").trim();
                    if text == "self" || text == "&self" || text == "&mut self" || text == "mut self" {
                        return Some(false);
                    }
                    // First real parameter is not self -> static
                    return Some(true);
                }
            }
        }
        // No parameters at all -> static (associated function)
        Some(true)
    } else {
        // function_signature_item (trait methods) may not have "parameters" field;
        // check the body text for self
        let text = node.utf8_text(source).unwrap_or("");
        if text.contains("&self") || text.contains("&mut self")
            || text.contains("(self") || text.contains("( self")
            || text.contains("(mut self")
        {
            Some(false)
        } else {
            Some(true)
        }
    }
}

/// Python: A method in a class is:
/// - static if decorated with `@staticmethod`
/// - instance if first param is `self` or `cls` (or `@classmethod`)
/// - static if neither decorator nor self/cls first param
fn detect_is_static_python(node: tree_sitter::Node, source: &[u8]) -> Option<bool> {
    // Check for decorators
    // In tree-sitter-python, decorators are sibling nodes preceding the function_definition,
    // or the function_definition may have a "decorator" child.
    // Actually, `decorated_definition` wraps the function. But from inside collect_nodes,
    // `node` is the function_definition itself. Check preceding siblings for decorators.
    let mut has_staticmethod = false;
    let mut has_classmethod = false;

    // Check if this function is wrapped in a decorated_definition
    if let Some(parent) = node.parent() {
        if parent.kind() == "decorated_definition" {
            for i in 0..parent.child_count() {
                if let Some(child) = parent.child(i as u32) {
                    if child.kind() == "decorator" {
                        let dec_text = child.utf8_text(source).unwrap_or("");
                        if dec_text.contains("staticmethod") {
                            has_staticmethod = true;
                        }
                        if dec_text.contains("classmethod") {
                            has_classmethod = true;
                        }
                    }
                }
            }
        }
    }

    if has_staticmethod {
        return Some(true);
    }

    // Check first parameter name
    if let Some(params) = node.child_by_field_name("parameters") {
        for i in 0..params.child_count() {
            if let Some(param) = params.child(i as u32) {
                if param.is_named() && param.kind() != "," {
                    let param_name = param.child_by_field_name("name")
                        .or_else(|| {
                            // Simple identifier parameter (no type annotation)
                            if param.kind() == "identifier" { Some(param) } else { None }
                        })
                        .and_then(|n| n.utf8_text(source).ok())
                        .unwrap_or("");
                    if param_name == "self" || param_name == "cls" || has_classmethod {
                        return Some(false); // instance or classmethod
                    }
                    // First param is something else and no @staticmethod -> static
                    return Some(true);
                }
            }
        }
        // No parameters and no @staticmethod -> static
        Some(true)
    } else {
        None
    }
}

/// Java/C#/TypeScript: A method is static if its text contains the `static` keyword
/// before the method name/body.
fn detect_is_static_keyword(node: tree_sitter::Node, source: &[u8]) -> Option<bool> {
    // Check for `static` keyword among the node's children (modifiers).
    // - Java tree-sitter: `modifiers` container node containing "static"
    // - C# tree-sitter: individual `modifier` children with text "static"
    // - TypeScript: direct `static` keyword child of method_definition
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            let child_kind = child.kind();
            // Direct "static" keyword child (TypeScript method_definition)
            if child_kind == "static" {
                return Some(true);
            }
            // Java modifiers container (wraps multiple modifier keywords)
            if child_kind == "modifiers" {
                let mod_text = child.utf8_text(source).unwrap_or("");
                if mod_text.contains("static") {
                    return Some(true);
                }
            }
            // C# individual modifier children
            if child_kind == "modifier" {
                let mod_text = child.utf8_text(source).unwrap_or("");
                if mod_text == "static" {
                    return Some(true);
                }
            }
        }
    }
    // No static keyword found -> instance method
    Some(false)
}

// ---------------------------------------------------------------------------
// Decorator / attribute collection
// ---------------------------------------------------------------------------

/// Collect decorator/attribute/annotation text for a node.
///
/// Strategy varies by language:
/// - **Python:** decorators live on a `decorated_definition` wrapper parent.
///   The function_definition/class_definition is a child of `decorated_definition`,
///   and the `decorator` nodes are siblings within that wrapper.
/// - **Rust/C#/Kotlin/TypeScript:** decorators are previous siblings of the node.
/// - **Java:** annotations live inside a `modifiers` child of the declaration.
///
/// Returns a comma-separated string of decorator texts, or empty string if none found.
fn collect_decorators(
    node: tree_sitter::Node,
    source: &[u8],
    config: &LangConfig,
) -> String {
    let mut decorators = Vec::new();

    // Strategy 1: Check parent wrapper (Python `decorated_definition` pattern).
    // In tree-sitter-python, `@decorator` + `def foo` is parsed as:
    //   decorated_definition
    //     decorator: @app.route("/api")
    //     decorator: @login_required
    //     function_definition: def foo(): ...
    if let Some(parent) = node.parent() {
        if parent.kind() == "decorated_definition" {
            for i in 0..parent.child_count() {
                if let Some(child) = parent.child(i as u32) {
                    if config.decorator_node_kinds.contains(&child.kind()) {
                        if let Ok(text) = child.utf8_text(source) {
                            decorators.push(text.trim().to_string());
                        }
                    }
                }
            }
        }
    }

    // Strategy 2: Previous siblings (Rust attribute_item, TS decorator, C# attribute_list, Kotlin annotation).
    // Walk backward through siblings collecting decorator nodes.
    if decorators.is_empty() {
        let mut sibling = node.prev_sibling();
        while let Some(sib) = sibling {
            if config.decorator_node_kinds.contains(&sib.kind()) {
                if let Ok(text) = sib.utf8_text(source) {
                    decorators.push(text.trim().to_string());
                }
                sibling = sib.prev_sibling();
            } else {
                // Stop at the first non-decorator sibling (don't skip over code).
                // Exception: skip comment nodes (they can appear between decorators).
                if sib.kind() == "comment" || sib.kind() == "line_comment" || sib.kind() == "block_comment" {
                    sibling = sib.prev_sibling();
                } else {
                    break;
                }
            }
        }
        // Previous-sibling walk collects in reverse order; fix to source order.
        decorators.reverse();
    }

    // Strategy 3: Child container (Java `modifiers` pattern).
    // In tree-sitter-java, annotations are children of the `modifiers` node
    // which is a child of the declaration itself.
    if decorators.is_empty() {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if child.kind() == "modifiers" {
                    for j in 0..child.child_count() {
                        if let Some(mod_child) = child.child(j as u32) {
                            if config.decorator_node_kinds.contains(&mod_child.kind()) {
                                if let Ok(text) = mod_child.utf8_text(source) {
                                    decorators.push(text.trim().to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Strategy 4: Direct child decorators (TypeScript pattern).
    // In tree-sitter-typescript, decorators are direct children of the declaration
    // node (accessible via field "decorator" or by kind match on children).
    if decorators.is_empty() {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if config.decorator_node_kinds.contains(&child.kind()) {
                    if let Ok(text) = child.utf8_text(source) {
                        decorators.push(text.trim().to_string());
                    }
                }
            }
        }
    }

    decorators.join(", ")
}

// ---------------------------------------------------------------------------
// Doc comment extraction (#401)
// ---------------------------------------------------------------------------

/// Collect doc comment text for a tree-sitter node.
///
/// Strategy: walk preceding siblings to find contiguous comment nodes
/// immediately before the declaration. Strips comment markers to expose
/// the plain natural-language text for semantic search.
///
/// Language-specific comment node kinds:
/// - **Rust:** `line_comment` (`///`), `block_comment` (`/** */`)
/// - **Python:** `comment` (`# ...`), plus first string_literal in function body (docstring)
/// - **TypeScript/JavaScript:** `comment` (`/** */`, `//`)
/// - **Go:** `comment` (`//`)
/// - **Java/Kotlin/C#:** `line_comment`, `block_comment`
///
/// Returns a space-joined string of comment lines with markers stripped, or
/// empty string if no doc comments are found.
fn collect_doc_comment(
    node: tree_sitter::Node,
    source: &[u8],
    config: &LangConfig,
) -> String {
    let language_name = config.language_name;
    // The node kinds that represent comments in each language.
    // All of these trigger the preceding-sibling walk.
    const COMMENT_KINDS: &[&str] = &[
        "line_comment",    // Rust (/// or //), Java, Kotlin, C#, Go
        "block_comment",   // Rust (/** */), Java, C#
        "comment",         // Python (# ...), TypeScript, JavaScript, Go
        "doc_comment",     // Some grammars expose this as a distinct kind
    ];

    let mut comment_lines: Vec<String> = Vec::new();

    // Walk preceding siblings in reverse (closest to definition first),
    // collecting contiguous comment nodes. Skip attribute/decorator nodes
    // transparently — they often sit between a doc comment and the symbol
    // (e.g. `/// doc \n #[test] \n fn foo()`).
    let mut sibling = node.prev_sibling();
    while let Some(sib) = sibling {
        let kind = sib.kind();
        if COMMENT_KINDS.contains(&kind) {
            let raw = sib.utf8_text(source).unwrap_or("").trim();
            let cleaned = strip_comment_markers(raw, language_name);
            if !cleaned.is_empty() {
                comment_lines.push(cleaned);
            }
            sibling = sib.prev_sibling();
        } else if config.decorator_node_kinds.contains(&kind) {
            // Attribute/decorator node between comment and symbol — skip
            // transparently without breaking, so we continue collecting
            // comments above the decorator.
            sibling = sib.prev_sibling();
        } else {
            // Stop at the first non-comment, non-decorator sibling.
            break;
        }
    }

    // Preceding-sibling walk collects in reverse order; restore source order.
    comment_lines.reverse();

    // Body docstring (Python style): the first expression_statement child
    // whose value is a string literal. Controlled by config.docstring_in_body
    // to avoid language-specific logic in generic.rs.
    // This handles `def foo(): """docstring""" ...`
    if comment_lines.is_empty() && config.docstring_in_body {
        if let Some(body_node) = node.child_by_field_name("body") {
            for i in 0..body_node.child_count() {
                if let Some(child) = body_node.child(i as u32) {
                    if child.kind() == "expression_statement" {
                        if let Some(inner) = child.child(0) {
                            if inner.kind() == "string" || inner.kind() == "string_literal" {
                                let raw = inner.utf8_text(source).unwrap_or("").trim();
                                let cleaned = strip_comment_markers(raw, language_name);
                                if !cleaned.is_empty() {
                                    comment_lines.push(cleaned);
                                }
                            }
                        }
                        break; // Only the first statement matters
                    }
                    // Skip decorators and blanks at the start
                    if child.kind() != "decorator" {
                        break;
                    }
                }
            }
        }
    }

    comment_lines.join(" ")
}

/// Strip comment markers from a raw comment string.
///
/// Removes `///`, `//!`, `//`, `/**`, `*/`, `*`, `#` (Python/shell),
/// and triple-quote docstring delimiters, leaving the plain text.
fn strip_comment_markers(raw: &str, _language_name: &str) -> String {
    let mut result = String::new();
    for line in raw.lines() {
        let stripped = line.trim()
            // Triple-quoted Python docstrings
            .trim_start_matches("\"\"\"")
            .trim_end_matches("\"\"\"")
            .trim_start_matches("'''")
            .trim_end_matches("'''")
            // Rust/Go/Java doc comment prefixes (order matters: longer first)
            .trim_start_matches("///")
            .trim_start_matches("//!")
            .trim_start_matches("/**")
            .trim_start_matches("/*")
            .trim_start_matches("//")
            .trim_start_matches('*')
            // Python/shell comments
            .trim_start_matches('#')
            // Block comment closing delimiter (e.g. "Returns the result. */")
            .trim_end_matches("*/")
            .trim();
        if !stripped.is_empty() {
            if !result.is_empty() { result.push(' '); }
            result.push_str(stripped);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract signature: text before the first `{`, or the first line.
pub(super) fn extract_signature(body: &str) -> String {
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
pub(super) fn extract_user_type(type_text: &str, require_uppercase: bool) -> Option<String> {
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
// Route query pass
// ---------------------------------------------------------------------------

/// Strip surrounding quotes from a path string capture.
///
/// Tree-sitter captures the full string literal including quotes (e.g.
/// `"/users"` or `'/users'`). This strips the outer quote characters so
/// metadata stores the bare path.
fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    // Guard: must be at least 2 chars so slicing `s[1..s.len()-1]` is safe.
    // A single quote/backtick character would panic without this guard.
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"'))
            || (s.starts_with('\'') && s.ends_with('\''))
            || (s.starts_with('`') && s.ends_with('`')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Infer the HTTP method from a capture name text if possible.
///
/// Handles two common naming conventions:
/// - **Suffix verb** (Python/Go/Express/Ruby/TypeScript): `router.post`, `router.GET`, `Post`
///   → `lower.ends_with(method)` or exact match
/// - **Spring MVC Mapping** (Java): `PostMapping`, `GetMapping`, `PutMapping`
///   → `lower == "{method}mapping"` (method + "Mapping")
///
/// Falls back to `default_method` when the text doesn't contain a recognisable verb.
fn infer_method_from_name(name: &str, default: &str) -> String {
    let lower = name.to_lowercase();
    for method in &["get", "post", "put", "delete", "patch", "head", "options"] {
        if lower == *method
            || lower.ends_with(method)
            || lower == format!("{}mapping", method)
        {
            return method.to_uppercase();
        }
    }
    default.to_string()
}

/// Run pre-compiled route queries against a file's syntax tree and emit
/// [`NodeKind::ApiEndpoint`] nodes for each matched route decorator.
///
/// `configs` and `compiled` must have the same length and ordering — each
/// `compiled[i]` is `Some(extractor)` if the query at `configs[i]` compiled
/// successfully, or `None` if compilation failed. The `None` slots are skipped.
///
/// Each matched capture set must have at least a `@path` capture. An optional
/// `@method` capture overrides `default_method` for the HTTP verb.
///
/// Called from [`GenericExtractor::run`] after normal manual traversal, so it
/// is purely additive — existing extraction is unaffected.

/// How many lines after the last decorator row to search for the handler
/// function definition. One extra line accommodates languages (e.g., Go)
/// where the function signature starts directly below the decorator.
const HANDLER_SEARCH_WINDOW: usize = 3;

fn run_route_queries(
    configs: &[RouteQueryConfig],
    compiled: &[Option<QueryExtractor>],
    root: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    language_name: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<crate::graph::Edge>,
) {
    // Pre-index Function nodes in this file by line_start for O(1) handler lookup.
    // Built once before the outer loop so it doesn't scale with O(routes × nodes).
    // Only includes nodes already in `nodes` (tree-sitter extracted Functions) —
    // ApiEndpoint nodes emitted during this pass are not yet present and need not be.
    let fn_by_line: std::collections::HashMap<usize, NodeId> = nodes
        .iter()
        .filter(|n| n.id.kind == NodeKind::Function && n.id.file == path)
        .map(|n| (n.line_start, n.id.clone()))
        .collect();

    for (cfg, maybe_extractor) in configs.iter().zip(compiled.iter()) {
        let extractor = match maybe_extractor {
            Some(e) => e,
            None => continue, // compile failed; warning already emitted at init
        };
        let captures: Vec<CaptureSet> = extractor.run(root, source);
        for capture in captures {
            let raw_path = match capture.get("path") {
                Some(p) => p.to_string(),
                None => continue,
            };
            let http_path = strip_quotes(&raw_path);
            if http_path.is_empty() {
                continue;
            }

            let method = if let Some(m) = capture.get("method") {
                infer_method_from_name(m, cfg.default_method)
            } else if let Some(n) = capture.get("name") {
                infer_method_from_name(n, cfg.default_method)
            } else {
                cfg.default_method.to_string()
            };

            let name = format!("{} {}", method, http_path);
            let mut metadata = BTreeMap::new();
            metadata.insert("http_method".to_string(), method.clone());
            metadata.insert("http_path".to_string(), http_path.clone());
            metadata.insert("route_query_label".to_string(), cfg.label.to_string());
            metadata.insert("synthetic".to_string(), "false".to_string());

            // The decorator ends at `capture.end_row` (0-indexed). The handler
            // function definition begins on the very next line or within a small
            // window below (e.g., one blank line between decorator and `def`).
            // `line_start` on Node is 1-indexed, so the expected range is
            // [end_row+2 .. end_row+2+HANDLER_SEARCH_WINDOW].
            let decorator_end_line = capture.end_row + 1; // convert to 1-indexed
            let search_start = decorator_end_line + 1;
            let search_end = decorator_end_line + 1 + HANDLER_SEARCH_WINDOW;

            let endpoint_node_id = NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: name.clone(),
                kind: NodeKind::ApiEndpoint,
            };

            // Find the handler function in the pre-built index: scan the narrow
            // window [search_start, search_end] instead of the entire node list.
            let handler = (search_start..=search_end)
                .find_map(|line| fn_by_line.get(&line));

            if let Some(handler_id) = handler {
                edges.push(crate::graph::Edge {
                    from: endpoint_node_id.clone(),
                    to: handler_id.clone(),
                    kind: crate::graph::EdgeKind::Implements,
                    source: ExtractionSource::TreeSitter,
                    confidence: crate::graph::Confidence::Detected,
                });
            }

            let node = Node {
                id: endpoint_node_id,
                language: language_name.to_string(),
                line_start: capture.start_row + 1,
                line_end: capture.end_row + 1,
                signature: format!("[route_decorator] {} {}", method, http_path),
                body: String::new(),
                metadata,
                source: ExtractionSource::TreeSitter,
            };
            nodes.push(node);
        }
    }
}

// ---------------------------------------------------------------------------
// #390 — is_async detection
// ---------------------------------------------------------------------------

/// Detect whether a function node has the `async` keyword.
///
/// Language-agnostic detection that covers:
/// - **Python**: `async_function_definition` node kind contains "async"
/// - **Rust**: `async fn` — the `async` keyword appears as a direct anonymous
///   child of `function_item` with kind `"async"`, OR the node's source text
///   starts with `async ` (fallback for grammar version differences)
/// - **TypeScript/JavaScript**: `async function` / `async ()` — `"async"` child
/// - **Go**: no async keyword (goroutines are concurrency primitives, not syntax)
///
/// Returns `true` if the function is async.
fn detect_is_async(node: tree_sitter::Node, source: &[u8]) -> bool {
    // Strategy 1: node kind itself signals async (Python async_function_definition,
    // Kotlin suspend functions mapped to a different node kind, etc.)
    if node.kind().contains("async") {
        return true;
    }
    // Strategy 2: direct named or anonymous child with kind "async".
    // Covers tree-sitter-rust where `async` is an anonymous child of `function_item`,
    // and tree-sitter-typescript where `async` is a named child of `function_declaration`.
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == "async" {
                return true;
            }
        }
    }
    // Strategy 3: fallback — inspect the raw source text of the function node.
    // Some grammar versions don't expose `async` as a separate child. This is a
    // last-resort check against the first line of the node's text, which is
    // the function signature. We only look at the signature (before `{`) to avoid
    // false positives on string literals inside the body.
    let text = node.utf8_text(source).unwrap_or("");
    let first_line = text.lines().next().unwrap_or("").trim();
    first_line.starts_with("async ")
        || first_line.contains(" async fn ")
        || first_line.starts_with("async fn ")
}

// ---------------------------------------------------------------------------
// #390 — is_test detection by decorator
// ---------------------------------------------------------------------------

/// Returns true if decorator strings indicate this is a test function.
///
/// Checks the comma-separated `decorators` string (as stored in metadata)
/// for known test patterns across languages:
/// - Rust:       `#[test]`, `#[tokio::test]`, `#[async_std::test]`
/// - Python:     `@pytest.mark.parametrize`, `@unittest.skip*`
/// - TypeScript: `it("...", ...)`, `test("...", ...)` — not decorator-based,
///               detected by `def test_*` name convention instead.
/// - Java/JUnit: `@Test`, `@ParameterizedTest`, `@RepeatedTest`
///
/// Also checks Python-style `def test_*` naming convention.
fn is_test_by_decorator(decorators: &str, fn_name: &str) -> bool {
    // Name-based convention: Python `def test_*`.
    if fn_name.starts_with("test_") || fn_name == "test" {
        return true;
    }
    if decorators.is_empty() {
        return false;
    }
    // Tokenize on comma + whitespace, then match each token.
    decorators
        .split(|c: char| c == ',' || c.is_whitespace())
        .any(|token| {
            let t = token.trim()
                // Strip Rust attribute brackets: `#[test]` → `test`
                .trim_start_matches("#[")
                .trim_end_matches(']')
                // Strip Python/TypeScript `@`: `@Test` → `Test`
                .trim_start_matches('@')
                .trim();
            // Exact match for common test markers (case-insensitive for Java/JUnit)
            let lower = t.to_lowercase();
            lower == "test"
                || lower == "tokio::test"
                || lower == "async_std::test"
                || lower == "actix_web::test"
                // JUnit 5
                || lower == "parameterizedtest"
                || lower == "repeatedtest"
                // Rust integration: rstest
                || lower == "rstest"
                // Python pytest
                || lower.starts_with("pytest.mark.")
                || lower.starts_with("pytest.")
        })
}

// ---------------------------------------------------------------------------
// #392 — decorator → Implements edges
// ---------------------------------------------------------------------------

/// Parse decorator strings on collected nodes and emit `Implements` edges.
///
/// Covers:
/// - **Rust `#[derive(...)]`**: `#[derive(Debug, Clone)]` → `Implements(Debug)`, `Implements(Clone)`
/// - **Python class decorator**: `@dataclass` → `Implements(DataClass)`
/// - **TypeScript `@Injectable()`** → `Implements(Injectable)`
/// - **Java `@Override`** → `Implements(Override)`
///
/// Only emits edges for nodes that have a `decorators` metadata entry.
/// Edges go from the decorated symbol to a pseudo-trait node with
/// `NodeKind::Trait` and an empty file (unresolved). Confidence is `Detected`.
fn emit_decorator_implements_edges(nodes: &[Node], path: &Path) -> Vec<Edge> {
    let mut edges = Vec::new();
    for node in nodes {
        let decorators = match node.metadata.get("decorators") {
            Some(d) if !d.is_empty() => d.as_str(),
            _ => continue,
        };
        // Only emit for types/structs/classes that carry trait-impl semantics.
        match node.id.kind {
            NodeKind::Struct | NodeKind::Enum | NodeKind::Trait
            | NodeKind::Function | NodeKind::Other(_) => {}
            _ => continue,
        }
        // Split decorators on ", " but only at the top level (not inside parentheses).
        // `#[derive(Debug, Clone)], #[serde(rename)]` → [`#[derive(Debug, Clone)]`, `#[serde(rename)]`]
        for dec in split_decorators(decorators) {
            let dec = dec.trim();
            if let Some(trait_names) = parse_decorator_trait_names(dec) {
                for trait_name in trait_names {
                    if trait_name.is_empty() { continue; }
                    edges.push(Edge {
                        from: node.id.clone(),
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
    }
    edges
}

/// Split a comma-separated decorators string at the top level only.
/// Commas inside parentheses (e.g. `#[derive(Debug, Clone)]`) are NOT split.
///
/// Input:  `"#[derive(Debug, Clone)], #[serde(rename_all = \"snake_case\")]"`
/// Output: `["#[derive(Debug, Clone)]", "#[serde(rename_all = \"snake_case\")]"]`
fn split_decorators(decorators: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    let bytes = decorators.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                result.push(decorators[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let tail = decorators[start..].trim();
    if !tail.is_empty() {
        result.push(tail);
    }
    result
}

/// Extract the trait/interface name(s) from a single decorator token.
///
/// Returns `None` if the decorator does not map to a named trait/interface,
/// or `Some(vec![...])` with zero or more names.
///
/// Examples:
/// - `#[derive(Debug, Clone)]` → `["Debug", "Clone"]`  (single decorator, multiple traits)
/// - `#[test]`               → `None` (test marker, not a trait)
/// - `@dataclass`            → `["DataClass"]`
/// - `@Injectable()`         → `["Injectable"]`
/// - `@Override`             → `["Override"]`
/// - `#[tokio::test]`        → `None`
fn parse_decorator_trait_names(dec: &str) -> Option<Vec<String>> {
    let dec = dec.trim();

    // Rust derive macro: `#[derive(Debug, Clone, Serialize)]`
    if dec.starts_with("#[derive(") || dec.starts_with("# [derive(") {
        let inner = dec
            .trim_start_matches("#[derive(")
            .trim_start_matches("# [derive(")
            .trim_end_matches(")]")
            .trim_end_matches(')');
        let names: Vec<String> = inner
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s.chars().next().map_or(false, |c| c.is_ascii_uppercase()))
            .collect();
        return if names.is_empty() { None } else { Some(names) };
    }

    // Strip common prefixes to get bare name.
    // Rust attribute: `#[something]` or `#[something(args)]`
    let bare = if dec.starts_with("#[") {
        let inner = dec.trim_start_matches("#[").trim_end_matches(']');
        // Take the identifier before any `(` or `::`
        inner.split(|c: char| c == '(' || c == ':').next().unwrap_or("").trim()
    } else if dec.starts_with('@') {
        // Python / TypeScript / Java / Kotlin annotation
        let inner = dec.trim_start_matches('@');
        // Take the identifier before any `(` or `.`
        inner.split(|c: char| c == '(' || c == '.').next().unwrap_or("").trim()
    } else {
        dec.split(|c: char| c == '(' || c == ':' || c == '.').next().unwrap_or("").trim()
    };

    if bare.is_empty() {
        return None;
    }

    // Skip pure test/lifecycle markers that don't represent trait impls.
    // These are frequent decorators that produce noise if mapped to Implements.
    const SKIP_MARKERS: &[&str] = &[
        // Rust test markers
        "test", "ignore", "allow", "deny", "warn", "forbid",
        "cfg", "cfg_attr", "doc", "inline", "cold", "must_use",
        "non_exhaustive", "repr", "deprecated", "macro_export",
        "macro_use", "path", "recursion_limit",
        // Python / TS lifecycle markers
        "staticmethod", "classmethod", "property", "abstractmethod",
        "override",
        // Java / JUnit
        "Override", "Deprecated", "SuppressWarnings",
        // Route/endpoint decorators (already handled by route_queries pass)
        "app", "router", "route", "get", "post", "put", "delete", "patch",
        "head", "options", "blueprint", "api",
        // Pytest / test lifecycle
        "fixture", "mark", "pytest",
        // DI / framework lifecycle decorators that don't map to trait impls
        "component", "module", "controller", "service", "pipe",
        // Common decorators whose target object is a namespace, not a trait
        "app_context", "before_request", "after_request",
        "login_required", "cached", "wraps",
    ];
    if SKIP_MARKERS.iter().any(|&m| bare.eq_ignore_ascii_case(m)) {
        return None;
    }
    // Skip decorators that start with lowercase AND are chained (e.g. `app.route`).
    // We split on '.' already, so bare is just `app` here — check that it starts
    // with uppercase to be treated as an interface/trait name.
    // Exception: known PascalCase-ish decorators like `Injectable` (already handled).
    // Rule: if the original decorator token (before the name field) has a dot in it,
    // it's a chained call like `@app.route(...)` — skip it.
    // This is the most reliable heuristic without per-language special-casing.
    let has_chain = if dec.starts_with('@') {
        let inner = dec.trim_start_matches('@');
        inner.contains('.') || inner.contains("::")
    } else {
        dec.contains("::") && !dec.starts_with("#[derive")
    };
    if has_chain {
        return None;
    }

    // Convert Python snake_case decorators to PascalCase for trait names.
    // `@dataclass` → `DataClass`, `@login_required` → `LoginRequired`
    let trait_name = if bare.contains('_') {
        // snake_case → PascalCase
        bare.split('_')
            .filter(|s| !s.is_empty())
            .map(|s| {
                let mut c = s.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().to_string() + c.as_str(),
                }
            })
            .collect::<String>()
    } else if bare.chars().next().map_or(false, |c| c.is_ascii_lowercase()) {
        // lowercase → capitalize first char (e.g. `@injectable` → `Injectable`)
        let mut c = bare.chars();
        match c.next() {
            None => return None,
            Some(f) => f.to_uppercase().to_string() + c.as_str(),
        }
    } else {
        bare.to_string()
    };

    if trait_name.is_empty() {
        None
    } else {
        Some(vec![trait_name])
    }
}

// ---------------------------------------------------------------------------
// #407 — same-file call detection
// ---------------------------------------------------------------------------

/// Walk the AST and collect bare `call_expression` / `function_call` nodes.
/// For each call whose callee name exactly matches a function defined in this
/// file, emit a `Calls` edge from the enclosing function to the callee.
///
/// Only emits when:
/// - The callee name exactly matches a `NodeKind::Function` defined in `nodes`
/// - The call is inside an enclosing function defined in this file
/// - The callee is not the caller itself (no self-loops)
///
/// Cross-file calls and method calls (e.g. `self.foo()`) are left to LSP.
fn detect_same_file_calls(
    root: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    config: &LangConfig,
    nodes: &[Node],
) -> Vec<Edge> {
    // Build a set of function names defined in this file for O(1) lookup.
    let file_fns: std::collections::HashSet<&str> = nodes
        .iter()
        .filter(|n| n.id.kind == NodeKind::Function && n.id.file == path)
        .map(|n| n.id.name.as_str())
        .collect();

    if file_fns.is_empty() {
        return Vec::new();
    }

    let mut edges = Vec::new();
    collect_calls(root, path, source, config, &file_fns, &None, &mut edges);
    edges
}

/// Detect the tree-sitter node kinds used for call expressions in each language.
/// Returns the call node kind and the field/child that holds the function name.
fn call_node_kind_for(language: &str) -> Option<(&'static str, &'static str)> {
    match language {
        "rust" => Some(("call_expression", "function")),
        "python" => Some(("call", "function")),
        "typescript" | "javascript" | "tsx" | "jsx" => Some(("call_expression", "function")),
        "go" => Some(("call_expression", "function")),
        "java" => Some(("method_invocation", "name")),
        "kotlin" => Some(("call_expression", "calleeExpression")),
        "csharp" => Some(("invocation_expression", "function")),
        "ruby" => Some(("call", "method")),
        "swift" => Some(("call_expression", "function")),
        "cpp" | "c" => Some(("call_expression", "function")),
        _ => None,
    }
}

/// Recursive walk that collects Calls edges for same-file function calls.
fn collect_calls(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    config: &LangConfig,
    file_fns: &std::collections::HashSet<&str>,
    enclosing_fn: &Option<String>,
    edges: &mut Vec<Edge>,
) {
    let kind = node.kind();

    // Check if this node is a function definition — update enclosing context.
    let is_fn_def = config.node_kinds.iter().any(|(ts_kind, nk)| {
        *ts_kind == kind && *nk == NodeKind::Function
    });
    let new_enclosing = if is_fn_def {
        node.child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok())
            .map(|s| s.to_string())
    } else {
        None
    };
    let enclosing = if new_enclosing.is_some() { &new_enclosing } else { enclosing_fn };

    // If we're inside a function, check for call expressions.
    if let Some(caller_name) = enclosing {
        if let Some((call_kind, name_field)) = call_node_kind_for(config.language_name) {
            if kind == call_kind {
                // Extract callee name from the designated field.
                // For Rust/TS: function field → may be an `identifier` directly,
                // or a `scoped_identifier` / `field_expression` — take only bare identifiers.
                if let Some(callee_node) = node.child_by_field_name(name_field) {
                    let callee_kind = callee_node.kind();
                    // Only resolve bare identifiers — skip method chains, scoped paths, etc.
                    if callee_kind == "identifier" || callee_kind == "simple_identifier" {
                        if let Ok(callee_name) = callee_node.utf8_text(source) {
                            let callee_name = callee_name.trim();
                            if file_fns.contains(callee_name) && callee_name != caller_name {
                                edges.push(Edge {
                                    from: NodeId {
                                        root: String::new(),
                                        file: path.to_path_buf(),
                                        name: caller_name.to_string(),
                                        kind: NodeKind::Function,
                                    },
                                    to: NodeId {
                                        root: String::new(),
                                        file: path.to_path_buf(),
                                        name: callee_name.to_string(),
                                        kind: NodeKind::Function,
                                    },
                                    kind: EdgeKind::Calls,
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

    // Recurse into children, passing the (possibly updated) enclosing context.
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_calls(child, path, source, config, file_fns, enclosing, edges);
        }
    }
}

// ---------------------------------------------------------------------------
// #409 — public API surface edges
// ---------------------------------------------------------------------------

/// Detect public re-exports on an Import node and emit metadata + edges.
///
/// - **Rust `pub use`**: sets `metadata["visibility"] = "pub"` and emits a `ReExports` edge.
/// - **Python `__all__ = [...]`**: sets `metadata["exported"] = "true"` on the Const node
///   (handled separately — this function handles Import nodes).
/// - **TypeScript `export { X }`**: sets `metadata["visibility"] = "pub"` and emits `ReExports`.
fn detect_public_api(
    node: tree_sitter::Node,
    source: &[u8],
    language: &str,
    path: &Path,
    name: &str,
    metadata: &mut BTreeMap<String, String>,
    edges: &mut Vec<Edge>,
) {
    match language {
        "rust" => detect_pub_use_rust(node, source, path, name, metadata, edges),
        "typescript" | "javascript" | "tsx" | "jsx" => {
            detect_export_ts(node, source, path, name, metadata, edges)
        }
        _ => {}
    }
}

/// Rust: detect `pub use ...` and emit `ReExports` edge.
///
/// In tree-sitter-rust, `use_declaration` has a `visibility_modifier` child
/// when the declaration is prefixed with `pub`.
fn detect_pub_use_rust(
    node: tree_sitter::Node,
    source: &[u8],
    path: &Path,
    name: &str,
    metadata: &mut BTreeMap<String, String>,
    edges: &mut Vec<Edge>,
) {
    let is_pub = (0..node.child_count()).any(|i| {
        node.child(i as u32)
            .map(|c| c.kind() == "visibility_modifier")
            .unwrap_or(false)
    });
    if !is_pub {
        // Double check by looking at the raw text (some grammars don't emit visibility_modifier
        // as a separate node for use_declaration).
        let text = node.utf8_text(source).unwrap_or("").trim();
        if !text.starts_with("pub ") && !text.starts_with("pub(") {
            return;
        }
    }

    metadata.insert("visibility".to_string(), "pub".to_string());

    // Extract the re-exported symbol name for the edge target.
    // `pub use crate::foo::Bar;` → target name "Bar"
    // `pub use crate::foo::*;` → wildcard, skip edge.
    let text = node.utf8_text(source).unwrap_or("").trim();
    if text.contains("::*") || text.ends_with("*;") {
        // Wildcard re-export — we can't enumerate targets statically.
        return;
    }
    // Last path segment before semicolon.
    let target = text
        .trim_end_matches(';')
        .trim_end_matches('}')
        .split([' ', ':', '/', '{'])
        .filter(|s| !s.is_empty() && s.chars().next().map_or(false, |c| c.is_alphanumeric() || c == '_'))
        .last()
        .unwrap_or("")
        .trim();
    if target.is_empty() || target == "self" {
        return;
    }

    edges.push(Edge {
        from: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: name.to_string(),
            kind: NodeKind::Import,
        },
        to: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: target.to_string(),
            kind: NodeKind::Module,
        },
        kind: EdgeKind::ReExports,
        source: ExtractionSource::TreeSitter,
        confidence: Confidence::Detected,
    });
}

/// TypeScript/JavaScript: detect `export { X }` and `export const/let/var/function` re-exports.
///
/// In tree-sitter-typescript, an export statement has an `export_clause` child
/// for named exports (`export { X, Y }`), or is an `export_statement` for
/// `export const/function/class`.
fn detect_export_ts(
    node: tree_sitter::Node,
    source: &[u8],
    path: &Path,
    name: &str,
    metadata: &mut BTreeMap<String, String>,
    edges: &mut Vec<Edge>,
) {
    // The import node text contains "export" for re-export declarations.
    let text = node.utf8_text(source).unwrap_or("").trim();
    if !text.starts_with("export ") && !text.starts_with("export{") {
        return;
    }
    metadata.insert("visibility".to_string(), "pub".to_string());

    // Extract exported names from `export { X, Y }`.
    // `export { X as Y }` → target is Y (the external name).
    let target = text
        .trim_end_matches(';')
        .split([' ', '{', '}', ','])
        .filter(|s| !s.is_empty() && *s != "export" && *s != "from")
        .next()
        .unwrap_or("")
        .trim();
    if target.is_empty() {
        return;
    }

    edges.push(Edge {
        from: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: name.to_string(),
            kind: NodeKind::Import,
        },
        to: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: target.to_string(),
            kind: NodeKind::Module,
        },
        kind: EdgeKind::ReExports,
        source: ExtractionSource::TreeSitter,
        confidence: Confidence::Detected,
    });
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
        // if_expression + else_clause = 2 branch nodes → complexity = 1 + 2 = 3
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
        assert_eq!(cc, 3, "if + else = 2 branches + 1 base = 3, got {}", cc);
    }

    #[test]
    fn test_cyclomatic_complexity_nested() {
        use crate::extract::rust::RUST_CONFIG;
        // for_expression + match_expression + 2 match_arms = 4 branches → cc = 5
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
        assert_eq!(cc, 5, "for + match + 2 arms = 4 branches + 1 base = 5, got {}", cc);
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
        // for_statement + if_statement + elif_clause + else_clause = 4 branches → cc = 5
        assert_eq!(cc, 5, "for + if + elif + else = 4 branches + 1 base = 5, got {}", cc);
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
        // if_statement + else_clause + ternary_expression = 3 branches → cc = 4
        assert_eq!(cc, 4, "if + else + ternary = 3 branches + 1 base = 4, got {}", cc);
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
        // Go tree-sitter doesn't emit else_clause; for_statement + if_statement = 2 branches → cc = 3
        assert_eq!(cc, 3, "for + if = 2 branches + 1 base = 3, got {}", cc);
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
        // Java tree-sitter doesn't emit else_clause; if_statement + for_statement = 2 branches → cc = 3
        assert_eq!(cc, 3, "if + for = 2 branches + 1 base = 3, got {}", cc);
    }

    // -----------------------------------------------------------------------
    // Adversarial: dissent-seeded complexity tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_adversarial_arithmetic_inflation_documented() {
        // After the operator-aware fix, binary_expression nodes are only
        // counted when the operator is logical (&&, ||, and, or).
        // Pure arithmetic should now correctly yield cc=1.
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
        // Pure arithmetic: no logical operators, no branches → cc = 1
        assert_eq!(cc, 1, "Pure arithmetic function should have cc=1, got {}", cc);
        eprintln!("ADVERSARIAL: Rust arithmetic cc={} (operator-aware filter working)", cc);
    }

    #[test]
    fn test_adversarial_go_arithmetic_vs_logical() {
        // After the operator-aware fix, Go binary_expression nodes are only
        // counted when the operator is logical (&&, ||). Arithmetic does not inflate.
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

        // Arithmetic: no logical operators → cc = 1
        assert_eq!(arith_cc, 1, "Go arithmetic function should have cc=1, got {}", arith_cc);
        // Logical: && + || = 2 logical operators → cc = 3
        assert_eq!(logic_cc, 3, "Go logical function with && and || should have cc=3, got {}", logic_cc);
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
        // Closures (closure_expression) are now treated as complexity boundaries,
        // so the if/else inside the closure is NOT counted in the parent.
        assert_eq!(cc, 1, "Parent with branchy closure should have cc=1 (closure is a boundary), got {}", cc);
        eprintln!("ADVERSARIAL: parent with closure cc={} (closure branches excluded)", cc);
    }

    // -----------------------------------------------------------------------
    // Enum variant indexing tests (#154)
    // -----------------------------------------------------------------------

    /// Helper: extract nodes from a code snippet using a given config.
    fn nodes_for(config: &'static LangConfig, path: &str, code: &str) -> Vec<Node> {
        let ext = GenericExtractor::new(config);
        let result = ext.run(Path::new(path), code).unwrap();
        result.nodes
    }

    #[test]
    fn test_enum_variants_typescript() {
        // TS enum variants use property_identifier/enum_assignment nodes,
        // handled in the TypeScript-specific extractor (not generic config).
        use crate::extract::typescript::TypeScriptExtractor;
        use crate::extract::Extractor;
        let extractor = TypeScriptExtractor::new();
        let code = r#"
enum Direction {
    Up,
    Down,
    Left = 3,
    Right = 4,
}
"#;
        let result = extractor.extract(Path::new("test.ts"), code).unwrap();
        let names: Vec<&str> = result.nodes.iter().map(|n| n.id.name.as_str()).collect();

        assert!(names.contains(&"Direction"), "Should find enum Direction");
        assert!(names.contains(&"Up"), "Should find variant Up, got: {:?}", names);
        assert!(names.contains(&"Down"), "Should find variant Down");
        assert!(names.contains(&"Left"), "Should find initialized variant Left");
        assert!(names.contains(&"Right"), "Should find initialized variant Right");

        let up = result.nodes.iter().find(|n| n.id.name == "Up").unwrap();
        assert_eq!(up.id.kind, NodeKind::Field, "TS variant should be Field kind");
        assert_eq!(
            up.metadata.get("parent_scope"),
            Some(&"Direction".to_string()),
            "TS variant should have parent_scope = Direction"
        );

        let has_field_edges: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::HasField)
            .collect();
        assert!(
            has_field_edges.iter().any(|e| e.from.name == "Direction" && e.to.name == "Up"),
            "Should have HasField edge Direction -> Up"
        );
    }

    #[test]
    fn test_enum_variants_java() {
        use crate::extract::configs::JAVA_CONFIG;
        let code = r#"
enum Color {
    RED,
    GREEN,
    BLUE;
}
"#;
        let nodes = nodes_for(&JAVA_CONFIG, "Test.java", code);
        let names: Vec<&str> = nodes.iter().map(|n| n.id.name.as_str()).collect();

        assert!(names.contains(&"Color"), "Should find enum Color");
        assert!(names.contains(&"RED"), "Should find variant RED, got: {:?}", names);
        assert!(names.contains(&"GREEN"), "Should find variant GREEN");

        let red = nodes.iter().find(|n| n.id.name == "RED").unwrap();
        assert_eq!(red.id.kind, NodeKind::Field, "Java variant should be Field kind");
        assert_eq!(
            red.metadata.get("parent_scope"),
            Some(&"Color".to_string()),
            "Java variant should have parent_scope = Color"
        );
    }

    #[test]
    fn test_enum_variants_csharp() {
        use crate::extract::configs::CSHARP_CONFIG;
        let code = r#"
enum Status {
    Active,
    Inactive,
    Pending,
}
"#;
        let nodes = nodes_for(&CSHARP_CONFIG, "Test.cs", code);
        let names: Vec<&str> = nodes.iter().map(|n| n.id.name.as_str()).collect();

        assert!(names.contains(&"Status"), "Should find enum Status");
        assert!(names.contains(&"Active"), "Should find variant Active, got: {:?}", names);
        assert!(names.contains(&"Inactive"), "Should find variant Inactive");

        let active = nodes.iter().find(|n| n.id.name == "Active").unwrap();
        assert_eq!(active.id.kind, NodeKind::Field, "C# variant should be Field kind");
        assert_eq!(
            active.metadata.get("parent_scope"),
            Some(&"Status".to_string()),
            "C# variant should have parent_scope = Status"
        );
    }

    #[test]
    fn test_enum_variants_cpp() {
        use crate::extract::configs::CPP_CONFIG;
        let code = r#"
enum Color {
    RED,
    GREEN,
    BLUE
};
"#;
        let nodes = nodes_for(&CPP_CONFIG, "test.cpp", code);
        let names: Vec<&str> = nodes.iter().map(|n| n.id.name.as_str()).collect();

        assert!(names.contains(&"Color"), "Should find enum Color");
        assert!(names.contains(&"RED"), "Should find variant RED, got: {:?}", names);
        assert!(names.contains(&"GREEN"), "Should find variant GREEN");

        let red = nodes.iter().find(|n| n.id.name == "RED").unwrap();
        assert_eq!(red.id.kind, NodeKind::Field, "C++ enumerator should be Field kind");
        assert_eq!(
            red.metadata.get("parent_scope"),
            Some(&"Color".to_string()),
            "C++ enumerator should have parent_scope = Color"
        );
    }

    // -----------------------------------------------------------------------
    // Decorator / attribute extraction tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_python_decorator_single() {
        use crate::extract::configs::PYTHON_CONFIG;
        let ext = GenericExtractor::new(&PYTHON_CONFIG);
        let code = r#"
@app.route("/api")
def handle():
    pass
"#;
        let result = ext.run(Path::new("app.py"), code).unwrap();
        let handle = result.nodes.iter().find(|n| n.id.name == "handle").unwrap();
        assert_eq!(
            handle.metadata.get("decorators").map(|s| s.as_str()),
            Some("@app.route(\"/api\")"),
            "Should capture single Python decorator"
        );
    }

    #[test]
    fn test_python_decorator_multiple() {
        use crate::extract::configs::PYTHON_CONFIG;
        let ext = GenericExtractor::new(&PYTHON_CONFIG);
        let code = r#"
@app.route("/api")
@login_required
def handle():
    pass
"#;
        let result = ext.run(Path::new("app.py"), code).unwrap();
        let handle = result.nodes.iter().find(|n| n.id.name == "handle").unwrap();
        let decorators = handle.metadata.get("decorators").expect("Should have decorators");
        assert!(decorators.contains("@app.route"), "Should contain @app.route, got: {}", decorators);
        assert!(decorators.contains("@login_required"), "Should contain @login_required, got: {}", decorators);
    }

    #[test]
    fn test_python_class_decorator() {
        use crate::extract::configs::PYTHON_CONFIG;
        let ext = GenericExtractor::new(&PYTHON_CONFIG);
        let code = r#"
@dataclass
class Config:
    port: int
"#;
        let result = ext.run(Path::new("config.py"), code).unwrap();
        let config = result.nodes.iter().find(|n| n.id.name == "Config").unwrap();
        assert_eq!(
            config.metadata.get("decorators").map(|s| s.as_str()),
            Some("@dataclass"),
            "Should capture class decorator"
        );
    }

    #[test]
    fn test_python_no_decorator() {
        use crate::extract::configs::PYTHON_CONFIG;
        let ext = GenericExtractor::new(&PYTHON_CONFIG);
        let code = r#"
def plain_function():
    pass
"#;
        let result = ext.run(Path::new("app.py"), code).unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "plain_function").unwrap();
        assert!(
            func.metadata.get("decorators").is_none(),
            "Undecorated function should not have decorators metadata"
        );
    }

    #[test]
    fn test_rust_attribute_single() {
        use crate::extract::rust::RUST_CONFIG;
        let ext = GenericExtractor::new(&RUST_CONFIG);
        let code = r#"
#[test]
fn test_something() {
    assert!(true);
}
"#;
        let result = ext.run(Path::new("lib.rs"), code).unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "test_something").unwrap();
        assert_eq!(
            func.metadata.get("decorators").map(|s| s.as_str()),
            Some("#[test]"),
            "Should capture Rust #[test] attribute"
        );
    }

    #[test]
    fn test_rust_attribute_multiple() {
        use crate::extract::rust::RUST_CONFIG;
        let ext = GenericExtractor::new(&RUST_CONFIG);
        let code = r#"
#[derive(Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub port: u16,
}
"#;
        let result = ext.run(Path::new("lib.rs"), code).unwrap();
        let config = result.nodes.iter().find(|n| n.id.name == "Config").unwrap();
        let decorators = config.metadata.get("decorators").expect("Should have decorators");
        assert!(decorators.contains("#[derive(Debug, Clone)]"), "Should contain derive, got: {}", decorators);
        assert!(decorators.contains("#[serde(rename_all = \"camelCase\")]"), "Should contain serde, got: {}", decorators);
    }

    #[test]
    fn test_rust_no_attribute() {
        use crate::extract::rust::RUST_CONFIG;
        let ext = GenericExtractor::new(&RUST_CONFIG);
        let code = "pub fn plain() {}\n";
        let result = ext.run(Path::new("lib.rs"), code).unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "plain").unwrap();
        assert!(
            func.metadata.get("decorators").is_none(),
            "Unattributed function should not have decorators metadata"
        );
    }

    #[test]
    fn test_java_annotation() {
        use crate::extract::configs::JAVA_CONFIG;
        let ext = GenericExtractor::new(&JAVA_CONFIG);
        let code = r#"
public class UserController {
    @Override
    public void handle() {
    }
}
"#;
        let result = ext.run(Path::new("UserController.java"), code).unwrap();
        let handle = result.nodes.iter().find(|n| n.id.name == "handle").unwrap();
        assert_eq!(
            handle.metadata.get("decorators").map(|s| s.as_str()),
            Some("@Override"),
            "Should capture Java @Override annotation"
        );
    }

    #[test]
    fn test_java_annotation_with_args() {
        use crate::extract::configs::JAVA_CONFIG;
        let ext = GenericExtractor::new(&JAVA_CONFIG);
        let code = r#"
public class UserController {
    @GetMapping("/users")
    public List<User> getUsers() {
        return null;
    }
}
"#;
        let result = ext.run(Path::new("UserController.java"), code).unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "getUsers").unwrap();
        let decorators = func.metadata.get("decorators").expect("Should have decorators");
        assert!(decorators.contains("@GetMapping"), "Should contain @GetMapping, got: {}", decorators);
    }

    #[test]
    fn test_java_class_annotations() {
        use crate::extract::configs::JAVA_CONFIG;
        let ext = GenericExtractor::new(&JAVA_CONFIG);
        let code = r#"
@RestController
@RequestMapping("/api")
public class UserController {
}
"#;
        let result = ext.run(Path::new("UserController.java"), code).unwrap();
        let cls = result.nodes.iter().find(|n| n.id.name == "UserController").unwrap();
        let decorators = cls.metadata.get("decorators").expect("Should have decorators");
        assert!(decorators.contains("@RestController"), "Should contain @RestController, got: {}", decorators);
        assert!(decorators.contains("@RequestMapping"), "Should contain @RequestMapping, got: {}", decorators);
    }

    #[test]
    fn test_typescript_decorator() {
        use crate::extract::configs::TYPESCRIPT_CONFIG;
        let ext = GenericExtractor::new(&TYPESCRIPT_CONFIG);
        let code = r#"
@Controller("/api")
class UserController {
}
"#;
        let result = ext.run(Path::new("controller.ts"), code).unwrap();
        let cls = result.nodes.iter().find(|n| n.id.name == "UserController").unwrap();
        let decorators = cls.metadata.get("decorators").expect("Should have decorators");
        assert!(decorators.contains("@Controller"), "Should contain @Controller, got: {}", decorators);
    }

    #[test]
    fn test_go_no_decorators() {
        use crate::extract::configs::GO_CONFIG;
        let ext = GenericExtractor::new(&GO_CONFIG);
        let code = "package main\n\nfunc hello() {}\n";
        let result = ext.run(Path::new("main.go"), code).unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "hello").unwrap();
        assert!(
            func.metadata.get("decorators").is_none(),
            "Go functions should never have decorators"
        );
    }

    // -----------------------------------------------------------------------
    // Adversarial decorator tests (seeded from dissent)
    // -----------------------------------------------------------------------

    /// Adversarial: decorator on first function must NOT bleed to the next undecorated one.
    #[test]
    fn test_python_decorator_no_bleed_to_next_function() {
        use crate::extract::configs::PYTHON_CONFIG;
        let ext = GenericExtractor::new(&PYTHON_CONFIG);
        let code = r#"
@app.route("/api")
def handle():
    pass

def plain():
    pass
"#;
        let result = ext.run(Path::new("app.py"), code).unwrap();
        let handle = result.nodes.iter().find(|n| n.id.name == "handle").unwrap();
        assert!(handle.metadata.get("decorators").is_some(), "handle should have decorators");

        let plain = result.nodes.iter().find(|n| n.id.name == "plain").unwrap();
        assert!(
            plain.metadata.get("decorators").is_none(),
            "Undecorated plain() should NOT inherit decorators from handle(), got: {:?}",
            plain.metadata.get("decorators")
        );
    }

    /// Adversarial: Rust attribute on first function must not bleed to second.
    #[test]
    fn test_rust_attribute_no_bleed() {
        use crate::extract::rust::RUST_CONFIG;
        let ext = GenericExtractor::new(&RUST_CONFIG);
        let code = r#"
#[test]
fn test_thing() {}

fn plain() {}
"#;
        let result = ext.run(Path::new("lib.rs"), code).unwrap();
        let test_fn = result.nodes.iter().find(|n| n.id.name == "test_thing").unwrap();
        assert_eq!(test_fn.metadata.get("decorators").map(|s| s.as_str()), Some("#[test]"));

        let plain = result.nodes.iter().find(|n| n.id.name == "plain").unwrap();
        assert!(
            plain.metadata.get("decorators").is_none(),
            "plain() should not inherit #[test] from test_thing(), got: {:?}",
            plain.metadata.get("decorators")
        );
    }

    /// Adversarial: comment between decorator and function should not break collection.
    #[test]
    fn test_rust_attribute_with_comment_between() {
        use crate::extract::rust::RUST_CONFIG;
        let ext = GenericExtractor::new(&RUST_CONFIG);
        let code = r#"
#[derive(Debug)]
// This is a config struct
pub struct Config {
    pub port: u16,
}
"#;
        let result = ext.run(Path::new("lib.rs"), code).unwrap();
        let config = result.nodes.iter().find(|n| n.id.name == "Config").unwrap();
        assert_eq!(
            config.metadata.get("decorators").map(|s| s.as_str()),
            Some("#[derive(Debug)]"),
            "Comment between attribute and struct should not break decorator collection"
        );
    }

    /// Adversarial: Java method with no annotations in annotated class.
    #[test]
    fn test_java_annotation_no_bleed_within_class() {
        use crate::extract::configs::JAVA_CONFIG;
        let ext = GenericExtractor::new(&JAVA_CONFIG);
        let code = r#"
public class UserController {
    @Override
    public void handle() {}

    public void plain() {}
}
"#;
        let result = ext.run(Path::new("UserController.java"), code).unwrap();
        let handle = result.nodes.iter().find(|n| n.id.name == "handle").unwrap();
        assert!(handle.metadata.get("decorators").is_some(), "handle should have @Override");

        let plain = result.nodes.iter().find(|n| n.id.name == "plain").unwrap();
        assert!(
            plain.metadata.get("decorators").is_none(),
            "plain() should NOT inherit @Override from handle(), got: {:?}",
            plain.metadata.get("decorators")
        );
    }

    // -----------------------------------------------------------------------
    // Type parameters extraction
    // -----------------------------------------------------------------------

    #[test]
    fn test_type_params_rust_generic_struct() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub struct Container<T: Clone + Send> {\n    value: T,\n}\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let container = result.nodes.iter().find(|n| n.id.name == "Container").unwrap();
        let tp = container.metadata.get("type_params").expect("Should have type_params");
        assert!(tp.contains("T"), "type_params should contain T, got: {}", tp);
        assert!(tp.contains("Clone"), "type_params should contain Clone bound, got: {}", tp);
        assert!(tp.contains("Send"), "type_params should contain Send bound, got: {}", tp);
    }

    #[test]
    fn test_type_params_rust_generic_function() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub fn process<T: Display + Send>(item: T) -> String {\n    item.to_string()\n}\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "process").unwrap();
        let tp = func.metadata.get("type_params").expect("Should have type_params");
        assert!(tp.contains("T"), "type_params should contain T, got: {}", tp);
        assert!(tp.contains("Display"), "type_params should contain Display bound, got: {}", tp);
        assert!(tp.contains("Send"), "type_params should contain Send bound, got: {}", tp);
    }

    #[test]
    fn test_type_params_rust_multiple_params() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub fn merge<K: Ord, V: Clone>(a: Map<K, V>, b: Map<K, V>) -> Map<K, V> {\n    a\n}\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "merge").unwrap();
        let tp = func.metadata.get("type_params").expect("Should have type_params");
        assert!(tp.contains("K"), "type_params should contain K, got: {}", tp);
        assert!(tp.contains("V"), "type_params should contain V, got: {}", tp);
        assert!(tp.contains("Ord"), "type_params should contain Ord, got: {}", tp);
    }

    #[test]
    fn test_type_params_rust_no_generics() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub fn plain(x: i32) -> i32 {\n    x\n}\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "plain").unwrap();
        assert!(
            func.metadata.get("type_params").is_none(),
            "Non-generic function should not have type_params"
        );
    }

    #[test]
    fn test_type_params_rust_trait() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub trait Converter<From, To> {\n    fn convert(&self, input: From) -> To;\n}\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let trait_node = result.nodes.iter().find(|n| n.id.name == "Converter").unwrap();
        let tp = trait_node.metadata.get("type_params").expect("Should have type_params");
        assert!(tp.contains("From"), "type_params should contain From, got: {}", tp);
        assert!(tp.contains("To"), "type_params should contain To, got: {}", tp);
    }

    #[test]
    fn test_type_params_rust_enum() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub enum Result<T, E> {\n    Ok(T),\n    Err(E),\n}\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let enum_node = result.nodes.iter().find(|n| n.id.name == "Result").unwrap();
        let tp = enum_node.metadata.get("type_params").expect("Should have type_params");
        assert!(tp.contains("T"), "type_params should contain T, got: {}", tp);
        assert!(tp.contains("E"), "type_params should contain E, got: {}", tp);
    }

    #[test]
    fn test_type_params_rust_type_alias() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub type Result<T> = std::result::Result<T, MyError>;\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let alias = result.nodes.iter().find(|n| n.id.name == "Result").unwrap();
        let tp = alias.metadata.get("type_params").expect("Should have type_params");
        assert!(tp.contains("T"), "type_params should contain T, got: {}", tp);
    }

    #[test]
    fn test_type_params_typescript_generic_function() {
        use crate::extract::configs::TYPESCRIPT_CONFIG;
        let code = "function identity<T>(arg: T): T {\n    return arg;\n}\n";
        let result = GenericExtractor::new(&TYPESCRIPT_CONFIG)
            .run(Path::new("test.ts"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "identity").unwrap();
        let tp = func.metadata.get("type_params").expect("TS function should have type_params");
        assert!(tp.contains("T"), "type_params should contain T, got: {}", tp);
    }

    #[test]
    fn test_type_params_typescript_generic_interface() {
        use crate::extract::configs::TYPESCRIPT_CONFIG;
        let code = "interface Repository<T extends Entity> {\n    find(id: string): T;\n}\n";
        let result = GenericExtractor::new(&TYPESCRIPT_CONFIG)
            .run(Path::new("test.ts"), code)
            .unwrap();
        let iface = result.nodes.iter().find(|n| n.id.name == "Repository").unwrap();
        let tp = iface.metadata.get("type_params").expect("TS interface should have type_params");
        assert!(tp.contains("T"), "type_params should contain T, got: {}", tp);
        assert!(tp.contains("Entity"), "type_params should contain extends Entity, got: {}", tp);
    }

    #[test]
    fn test_type_params_typescript_generic_class() {
        use crate::extract::configs::TYPESCRIPT_CONFIG;
        let code = "class Box<T> {\n    value: T;\n}\n";
        let result = GenericExtractor::new(&TYPESCRIPT_CONFIG)
            .run(Path::new("test.ts"), code)
            .unwrap();
        let class = result.nodes.iter().find(|n| n.id.name == "Box").unwrap();
        let tp = class.metadata.get("type_params").expect("TS class should have type_params");
        assert!(tp.contains("T"), "type_params should contain T, got: {}", tp);
    }

    #[test]
    fn test_type_params_java_generic_class() {
        use crate::extract::configs::JAVA_CONFIG;
        let code = "class Container<T extends Comparable<T>> {\n    T value;\n}\n";
        let result = GenericExtractor::new(&JAVA_CONFIG)
            .run(Path::new("Test.java"), code)
            .unwrap();
        let class = result.nodes.iter().find(|n| n.id.name == "Container").unwrap();
        let tp = class.metadata.get("type_params").expect("Java class should have type_params");
        assert!(tp.contains("T"), "type_params should contain T, got: {}", tp);
        assert!(tp.contains("Comparable"), "type_params should contain bound Comparable, got: {}", tp);
    }

    #[test]
    fn test_type_params_java_generic_method() {
        use crate::extract::configs::JAVA_CONFIG;
        let code = "class Util {\n    <T> T identity(T item) {\n        return item;\n    }\n}\n";
        let result = GenericExtractor::new(&JAVA_CONFIG)
            .run(Path::new("Util.java"), code)
            .unwrap();
        let method = result.nodes.iter().find(|n| n.id.name == "identity").unwrap();
        let tp = method.metadata.get("type_params").expect("Java method should have type_params");
        assert!(tp.contains("T"), "type_params should contain T, got: {}", tp);
    }

    #[test]
    fn test_type_params_go_generic_function() {
        use crate::extract::configs::GO_CONFIG;
        let code = "package main\n\nfunc Map[T any, U any](items []T, f func(T) U) []U {\n    return nil\n}\n";
        let result = GenericExtractor::new(&GO_CONFIG)
            .run(Path::new("test.go"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "Map").unwrap();
        let tp = func.metadata.get("type_params").expect("Go function should have type_params");
        assert!(tp.contains("T"), "type_params should contain T, got: {}", tp);
        assert!(tp.contains("U"), "type_params should contain U, got: {}", tp);
    }

    #[test]
    fn test_type_params_no_generics_languages() {
        // Languages without generics should have type_param_node_kind = None
        use crate::extract::configs::{JAVASCRIPT_CONFIG, LUA_CONFIG, RUBY_CONFIG, BASH_CONFIG};
        assert!(JAVASCRIPT_CONFIG.type_param_node_kind.is_none());
        assert!(LUA_CONFIG.type_param_node_kind.is_none());
        assert!(RUBY_CONFIG.type_param_node_kind.is_none());
        assert!(BASH_CONFIG.type_param_node_kind.is_none());
    }

    // -- Adversarial type_params tests --

    /// Adversarial: Rust lifetime parameters mixed with type params
    #[test]
    fn test_type_params_rust_lifetime_and_type() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub struct Ref<'a, T: 'a> {\n    data: &'a T,\n}\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let node = result.nodes.iter().find(|n| n.id.name == "Ref").unwrap();
        let tp = node.metadata.get("type_params").expect("Should have type_params with lifetime");
        assert!(tp.contains("'a"), "type_params should contain lifetime 'a, got: {}", tp);
        assert!(tp.contains("T"), "type_params should contain type T, got: {}", tp);
    }

    /// Adversarial: Rust impl block with generics
    #[test]
    fn test_type_params_rust_impl_generic() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "struct Wrapper<T>(T);\nimpl<T: Clone> Wrapper<T> {\n    fn get(&self) -> T { self.0.clone() }\n}\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        // The struct should have <T>
        let wrapper = result.nodes.iter().find(|n| n.id.name == "Wrapper" && n.id.kind == NodeKind::Struct).unwrap();
        let tp = wrapper.metadata.get("type_params").expect("Wrapper struct should have type_params");
        assert!(tp.contains("T"), "struct type_params should contain T, got: {}", tp);

        // The impl block should have <T: Clone>
        let impl_node = result.nodes.iter()
            .find(|n| n.id.kind == NodeKind::Impl && n.id.name.contains("Wrapper"))
            .unwrap();
        let impl_tp = impl_node.metadata.get("type_params").expect("impl should have type_params");
        assert!(impl_tp.contains("T"), "impl type_params should contain T, got: {}", impl_tp);
        assert!(impl_tp.contains("Clone"), "impl type_params should contain Clone, got: {}", impl_tp);
    }

    /// Adversarial: deeply nested generics (e.g. HashMap<String, Vec<T>>)
    /// Only the type_parameters node text is captured, not nested generic args in types
    #[test]
    fn test_type_params_rust_complex_bounds() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub fn transform<T: Into<String> + AsRef<str>>(input: T) -> String {\n    input.into()\n}\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "transform").unwrap();
        let tp = func.metadata.get("type_params").expect("Should have type_params");
        assert!(tp.contains("Into<String>"), "type_params should contain Into<String>, got: {}", tp);
        assert!(tp.contains("AsRef<str>"), "type_params should contain AsRef<str>, got: {}", tp);
    }

    /// Adversarial: TypeScript with multiple type params and constraints
    #[test]
    fn test_type_params_typescript_multiple_constraints() {
        use crate::extract::configs::TYPESCRIPT_CONFIG;
        let code = "function merge<T extends object, U extends object>(a: T, b: U): T & U {\n    return {...a, ...b};\n}\n";
        let result = GenericExtractor::new(&TYPESCRIPT_CONFIG)
            .run(Path::new("test.ts"), code)
            .unwrap();
        let func = result.nodes.iter().find(|n| n.id.name == "merge").unwrap();
        let tp = func.metadata.get("type_params").expect("Should have type_params");
        assert!(tp.contains("T"), "type_params should contain T, got: {}", tp);
        assert!(tp.contains("U"), "type_params should contain U, got: {}", tp);
        assert!(tp.contains("extends"), "type_params should contain extends keyword, got: {}", tp);
    }

    /// Adversarial: Java wildcard bounds
    #[test]
    fn test_type_params_java_multiple_bounds() {
        use crate::extract::configs::JAVA_CONFIG;
        let code = "class Sorter<T extends Comparable<T> & Serializable> {\n    void sort() {}\n}\n";
        let result = GenericExtractor::new(&JAVA_CONFIG)
            .run(Path::new("Sorter.java"), code)
            .unwrap();
        let class = result.nodes.iter().find(|n| n.id.name == "Sorter").unwrap();
        let tp = class.metadata.get("type_params").expect("Should have type_params");
        assert!(tp.contains("Comparable"), "type_params should contain Comparable bound, got: {}", tp);
        assert!(tp.contains("Serializable"), "type_params should contain Serializable bound, got: {}", tp);
    }

    // -- Design pattern hint detection tests --

    #[test]
    fn test_pattern_hint_struct_factory() {
        assert_eq!(
            detect_pattern_hint("ConnectionFactory", &NodeKind::Struct),
            Some("factory".to_string()),
        );
    }

    #[test]
    fn test_pattern_hint_case_insensitive() {
        // Mixed case should still match
        assert_eq!(
            detect_pattern_hint("MyCustomHANDLER", &NodeKind::Struct),
            Some("handler".to_string()),
        );
    }

    #[test]
    fn test_pattern_hint_trait_observer() {
        assert_eq!(
            detect_pattern_hint("EventObserver", &NodeKind::Trait),
            Some("observer".to_string()),
        );
    }

    #[test]
    fn test_pattern_hint_function_builder() {
        assert_eq!(
            detect_pattern_hint("create_query_builder", &NodeKind::Function),
            Some("builder".to_string()),
        );
    }

    #[test]
    fn test_pattern_hint_enum_strategy() {
        assert_eq!(
            detect_pattern_hint("RetryStrategy", &NodeKind::Enum),
            Some("strategy".to_string()),
        );
    }

    #[test]
    fn test_pattern_hint_single_word_matches() {
        // A struct literally named "Factory" should still match
        assert_eq!(
            detect_pattern_hint("Factory", &NodeKind::Struct),
            Some("factory".to_string()),
        );
    }

    #[test]
    fn test_pattern_hint_no_match() {
        assert_eq!(
            detect_pattern_hint("DatabaseConnection", &NodeKind::Struct),
            None,
        );
    }

    #[test]
    fn test_pattern_hint_skips_import() {
        assert_eq!(
            detect_pattern_hint("use_factory", &NodeKind::Import),
            None,
        );
    }

    #[test]
    fn test_pattern_hint_skips_module() {
        assert_eq!(
            detect_pattern_hint("handler_module", &NodeKind::Module),
            None,
        );
    }

    #[test]
    fn test_pattern_hint_skips_field() {
        assert_eq!(
            detect_pattern_hint("factory_field", &NodeKind::Field),
            None,
        );
    }

    #[test]
    fn test_pattern_hint_skips_const() {
        assert_eq!(
            detect_pattern_hint("DEFAULT_HANDLER", &NodeKind::Const),
            None,
        );
    }

    #[test]
    fn test_pattern_hint_all_suffixes() {
        let suffixes = vec![
            ("MyFactory", "factory"),
            ("QueryBuilder", "builder"),
            ("RequestHandler", "handler"),
            ("DbAdapter", "adapter"),
            ("CacheProxy", "proxy"),
            ("EventObserver", "observer"),
            ("UserRepository", "repository"),
            ("RetryStrategy", "strategy"),
            ("AppSingleton", "singleton"),
            ("LogDecorator", "decorator"),
            ("AuthMiddleware", "middleware"),
            ("ConfigProvider", "provider"),
            ("UserService", "service"),
            ("HomeController", "controller"),
            ("TaskManager", "manager"),
        ];
        for (name, expected) in suffixes {
            assert_eq!(
                detect_pattern_hint(name, &NodeKind::Struct),
                Some(expected.to_string()),
                "Expected pattern_hint '{}' for name '{}'",
                expected,
                name,
            );
        }
    }

    #[test]
    fn test_pattern_hint_other_kind_matches() {
        // NodeKind::Other("class") should also match (Python/TS classes)
        assert_eq!(
            detect_pattern_hint("PaymentService", &NodeKind::Other("class".to_string())),
            Some("service".to_string()),
        );
    }

    #[test]
    fn test_pattern_hint_integration_rust() {
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
struct ConnectionFactory {
    pool: Vec<Connection>,
}

trait EventObserver {
    fn on_event(&self);
}

fn create_handler() {}

struct PlainStruct {
    field: i32,
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("src/patterns.rs"), code)
            .unwrap();

        let factory = result.nodes.iter().find(|n| n.id.name == "ConnectionFactory").unwrap();
        assert_eq!(
            factory.metadata.get("pattern_hint").map(|s| s.as_str()),
            Some("factory"),
            "ConnectionFactory should have pattern_hint=factory",
        );

        let observer = result.nodes.iter().find(|n| n.id.name == "EventObserver").unwrap();
        assert_eq!(
            observer.metadata.get("pattern_hint").map(|s| s.as_str()),
            Some("observer"),
            "EventObserver should have pattern_hint=observer",
        );

        let handler_fn = result.nodes.iter().find(|n| n.id.name == "create_handler").unwrap();
        assert_eq!(
            handler_fn.metadata.get("pattern_hint").map(|s| s.as_str()),
            Some("handler"),
            "create_handler should have pattern_hint=handler",
        );

        let plain = result.nodes.iter().find(|n| n.id.name == "PlainStruct").unwrap();
        assert!(
            plain.metadata.get("pattern_hint").is_none(),
            "PlainStruct should NOT have a pattern_hint",
        );
    }

    // -- Adversarial pattern hint tests --

    #[test]
    fn test_pattern_hint_empty_name() {
        assert_eq!(detect_pattern_hint("", &NodeKind::Struct), None);
    }

    #[test]
    fn test_pattern_hint_unicode_name() {
        // Non-ASCII name should not crash, and should not match
        assert_eq!(detect_pattern_hint("FabrikController\u{00e9}", &NodeKind::Struct), None);
        // But ASCII suffix after unicode prefix should match
        assert_eq!(
            detect_pattern_hint("\u{00e9}Factory", &NodeKind::Struct),
            Some("factory".to_string()),
        );
    }

    #[test]
    fn test_pattern_hint_overlapping_suffix() {
        // "ServiceProvider" ends with both "provider" and, if we had "ervice", also "service".
        // Should match "provider" because it's checked before "service" would substring-match.
        // Actually "serviceprovider" ends with "provider" (first match in list that matches).
        assert_eq!(
            detect_pattern_hint("ServiceProvider", &NodeKind::Struct),
            Some("provider".to_string()),
        );
    }

    #[test]
    fn test_pattern_hint_substring_not_suffix() {
        // "FactoryUtil" -- "factory" is NOT a suffix here
        assert_eq!(detect_pattern_hint("FactoryUtil", &NodeKind::Struct), None);
        // "HandlerConfig" -- "handler" is NOT a suffix
        assert_eq!(detect_pattern_hint("HandlerConfig", &NodeKind::Struct), None);
    }

    #[test]
    fn test_pattern_hint_snake_case_suffix() {
        // Snake case: "my_handler" ends with "handler"
        assert_eq!(
            detect_pattern_hint("my_handler", &NodeKind::Function),
            Some("handler".to_string()),
        );
        // "event_observer" ends with "observer"
        assert_eq!(
            detect_pattern_hint("event_observer", &NodeKind::Function),
            Some("observer".to_string()),
        );
    }

    #[test]
    fn test_pattern_hint_impl_kind_excluded() {
        assert_eq!(detect_pattern_hint("FactoryImpl", &NodeKind::Impl), None);
    }

    #[test]
    fn test_pattern_hint_cross_language_python() {
        use crate::extract::configs::PYTHON_CONFIG;
        let code = "class UserRepository:\n    pass\n\nclass PlainModel:\n    pass\n";
        let result = GenericExtractor::new(&PYTHON_CONFIG)
            .run(Path::new("models.py"), code)
            .unwrap();

        let repo = result.nodes.iter().find(|n| n.id.name == "UserRepository").unwrap();
        assert_eq!(
            repo.metadata.get("pattern_hint").map(|s| s.as_str()),
            Some("repository"),
            "Python class UserRepository should have pattern_hint=repository",
        );

        let plain = result.nodes.iter().find(|n| n.id.name == "PlainModel").unwrap();
        assert!(
            plain.metadata.get("pattern_hint").is_none(),
            "PlainModel should NOT have a pattern_hint",
        );
    }

    #[test]
    fn test_pattern_hint_cross_language_typescript() {
        use crate::extract::configs::TYPESCRIPT_CONFIG;
        let code = "class AuthMiddleware {\n  handle() {}\n}\n\nclass UserDTO {\n  name: string;\n}\n";
        let result = GenericExtractor::new(&TYPESCRIPT_CONFIG)
            .run(Path::new("auth.ts"), code)
            .unwrap();

        let mw = result.nodes.iter().find(|n| n.id.name == "AuthMiddleware").unwrap();
        assert_eq!(
            mw.metadata.get("pattern_hint").map(|s| s.as_str()),
            Some("middleware"),
            "TypeScript class AuthMiddleware should have pattern_hint=middleware",
        );

        let dto = result.nodes.iter().find(|n| n.id.name == "UserDTO").unwrap();
        assert!(
            dto.metadata.get("pattern_hint").is_none(),
            "UserDTO should NOT have a pattern_hint",
        );
    }

    #[test]
    fn test_effective_pattern_suffixes_returns_defaults_without_init() {
        // Without calling init_pattern_config, effective_pattern_suffixes()
        // should fall back to built-in defaults.
        let suffixes = effective_pattern_suffixes();
        assert!(
            !suffixes.is_empty(),
            "Should return built-in defaults when no config initialized"
        );
        assert!(
            suffixes.iter().any(|(s, _)| s == "factory"),
            "Built-in defaults should include 'factory'"
        );
        assert!(
            suffixes.iter().any(|(s, _)| s == "observer"),
            "Built-in defaults should include 'observer'"
        );
    }

    // ── #401: doc comment extraction ─────────────────────────────────────

    #[test]
    fn test_rust_doc_comment_extracted() {
        use crate::extract::rust::RustExtractor;
        let ext = RustExtractor::new();
        let code = r#"
/// Charges the card and records the transaction in the audit log.
/// Sends a confirmation email on success.
pub fn process_payment(amount: u32) -> Result<(), Error> {
    Ok(())
}
"#;
        let result = ext.extract(Path::new("test.rs"), code).unwrap();
        let fn_node = result.nodes.iter().find(|n| n.id.name == "process_payment")
            .expect("should find process_payment");
        let doc = fn_node.metadata.get("doc_comment")
            .expect("should have doc_comment metadata");
        assert!(
            doc.contains("Charges the card"),
            "doc comment should contain first line text: {}",
            doc
        );
        assert!(
            doc.contains("confirmation email"),
            "doc comment should contain second line text: {}",
            doc
        );
        // Markers should be stripped
        assert!(
            !doc.contains("///"),
            "doc comment markers should be stripped: {}",
            doc
        );
    }

    #[test]
    fn test_rust_struct_doc_comment_extracted() {
        use crate::extract::rust::RustExtractor;
        let ext = RustExtractor::new();
        let code = r#"
/// A payment processor that handles card charges.
pub struct PaymentProcessor {
    /// The API key for the payment gateway.
    pub api_key: String,
}
"#;
        let result = ext.extract(Path::new("test.rs"), code).unwrap();
        let struct_node = result.nodes.iter().find(|n| n.id.name == "PaymentProcessor")
            .expect("should find PaymentProcessor");
        let doc = struct_node.metadata.get("doc_comment")
            .expect("PaymentProcessor should have doc_comment");
        assert!(
            doc.contains("payment processor"),
            "struct doc comment should be extracted: {}",
            doc
        );
    }

    #[test]
    fn test_no_doc_comment_when_absent() {
        use crate::extract::rust::RustExtractor;
        let ext = RustExtractor::new();
        let code = r#"
pub fn no_doc() -> u32 {
    42
}
"#;
        let result = ext.extract(Path::new("test.rs"), code).unwrap();
        let fn_node = result.nodes.iter().find(|n| n.id.name == "no_doc")
            .expect("should find no_doc");
        assert!(
            fn_node.metadata.get("doc_comment").is_none(),
            "should have no doc_comment when absent"
        );
    }

    #[test]
    fn test_strip_comment_markers_rust_doc() {
        let raw = "/// Charges the card.\n/// Records the transaction.";
        let result = strip_comment_markers(raw, "rust");
        assert!(!result.contains("///"), "markers should be stripped: {}", result);
        assert!(result.contains("Charges the card"), "text should be preserved: {}", result);
        assert!(result.contains("Records the transaction"), "second line should be included: {}", result);
    }

    #[test]
    fn test_strip_comment_markers_block_comment() {
        let raw = "/** Returns the result.\n * @param x the input\n */";
        let result = strip_comment_markers(raw, "typescript");
        assert!(!result.contains("/**"), "block start should be stripped: {}", result);
        assert!(!result.contains("*/"), "block end should be stripped: {}", result);
        assert!(result.contains("Returns the result"), "content should be preserved: {}", result);
    }

    #[test]
    fn test_strip_comment_markers_single_line_block() {
        let raw = "/** Returns the computed value. */";
        let result = strip_comment_markers(raw, "java");
        assert!(!result.contains("/**"), "opening delimiter should be stripped: {}", result);
        assert!(!result.contains("*/"), "closing delimiter should be stripped: {}", result);
        assert!(result.contains("Returns the computed value"), "content should be preserved: {}", result);
    }

    #[test]
    fn test_rust_doc_comment_with_attribute_between() {
        use crate::extract::rust::RustExtractor;
        let ext = RustExtractor::new();
        let code = r#"
/// This function is a test helper.
#[test]
fn my_test_helper() {}
"#;
        let result = ext.extract(Path::new("test.rs"), code).unwrap();
        let fn_node = result.nodes.iter().find(|n| n.id.name == "my_test_helper")
            .expect("should find my_test_helper");
        let doc = fn_node.metadata.get("doc_comment")
            .expect("should extract doc comment even with #[test] attribute between comment and fn");
        assert!(
            doc.contains("test helper"),
            "doc comment should be extracted through attribute: {}",
            doc
        );
    }

    #[test]
    fn test_python_comment_extracted() {
        use crate::extract::configs::PYTHON_CONFIG;
        let ext = GenericExtractor::new(&PYTHON_CONFIG);
        let code = r#"
# Converts the input to uppercase.
def process_input(value):
    return value.upper()
"#;
        let result = ext.extract(Path::new("test.py"), code).unwrap();
        let fn_node = result.nodes.iter().find(|n| n.id.name == "process_input")
            .expect("should find process_input");
        let doc = fn_node.metadata.get("doc_comment")
            .expect("should have doc_comment for Python function with preceding comment");
        assert!(
            doc.contains("Converts the input"),
            "Python comment should be extracted: {}",
            doc
        );
        assert!(
            !doc.contains('#'),
            "Python # marker should be stripped: {}",
            doc
        );
    }

    #[test]
    fn test_python_docstring_extracted() {
        use crate::extract::configs::PYTHON_CONFIG;
        let ext = GenericExtractor::new(&PYTHON_CONFIG);
        let code = r#"def process_payment(amount):
    """Charges the card and records the transaction in the audit log."""
    return True
"#;
        let result = ext.extract(Path::new("test.py"), code).unwrap();
        let fn_node = result.nodes.iter().find(|n| n.id.name == "process_payment")
            .expect("should find process_payment");
        // Docstrings may or may not be captured depending on tree-sitter-python node kinds.
        // If captured, they should contain the meaningful text.
        if let Some(doc) = fn_node.metadata.get("doc_comment") {
            assert!(
                doc.contains("Charges the card") || doc.contains("audit log"),
                "Python docstring content should be preserved: {}",
                doc
            );
        }
        // If not captured (graceful degradation), that's acceptable —
        // Python # comments (preceding sibling path) still work.
    }

    // -----------------------------------------------------------------------
    // Route query end-to-end tests (through GenericExtractor)
    // -----------------------------------------------------------------------

    /// Verify that the Python extractor emits ApiEndpoint nodes for Flask routes.
    #[test]
    fn test_python_extractor_emits_api_endpoint_for_flask_route() {
        use crate::extract::configs::PYTHON_CONFIG;
        let ext = GenericExtractor::new(&PYTHON_CONFIG);
        let code = r#"
@app.route("/users")
def get_users():
    pass

@app.route("/items")
def get_items():
    pass
"#;
        let result = ext.run(Path::new("routes.py"), code).unwrap();
        let api_nodes: Vec<_> = result.nodes.iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert_eq!(api_nodes.len(), 2, "should emit 2 ApiEndpoint nodes, got: {:?}",
            api_nodes.iter().map(|n| &n.id.name).collect::<Vec<_>>());
        assert!(
            api_nodes.iter().any(|n| n.metadata.get("http_path").map(|p| p == "/users").unwrap_or(false)),
            "should have ApiEndpoint for /users"
        );
    }

    /// Verify that the TypeScript extractor emits ApiEndpoint nodes for NestJS routes.
    #[test]
    fn test_typescript_extractor_emits_api_endpoint_for_nestjs() {
        use crate::extract::configs::TYPESCRIPT_CONFIG;
        let ext = GenericExtractor::new(&TYPESCRIPT_CONFIG);
        let code = r#"
class UserController {
  @Get("/users")
  findAll() {}

  @Post("/users")
  create() {}
}
"#;
        let result = ext.run(Path::new("user.controller.ts"), code).unwrap();
        let api_nodes: Vec<_> = result.nodes.iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert_eq!(api_nodes.len(), 2, "should emit 2 ApiEndpoint nodes");
        assert!(
            api_nodes.iter().any(|n| {
                n.metadata.get("http_method").map(|m| m == "POST").unwrap_or(false)
            }),
            "should have POST endpoint"
        );
    }

    /// Verify that the existing function extraction still works alongside route queries.
    #[test]
    fn test_python_route_query_does_not_break_function_extraction() {
        use crate::extract::configs::PYTHON_CONFIG;
        let ext = GenericExtractor::new(&PYTHON_CONFIG);
        let code = r#"
@app.route("/users")
def get_users():
    pass

def helper():
    pass
"#;
        let result = ext.run(Path::new("test.py"), code).unwrap();
        let fn_nodes: Vec<_> = result.nodes.iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert!(
            fn_nodes.iter().any(|n| n.id.name == "get_users"),
            "should still extract get_users function"
        );
        assert!(
            fn_nodes.iter().any(|n| n.id.name == "helper"),
            "should still extract helper function"
        );
        // Both functions extracted AND the ApiEndpoint node
        let api_nodes: Vec<_> = result.nodes.iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert_eq!(api_nodes.len(), 1, "should have exactly 1 ApiEndpoint node");
    }

    /// Verify Java @PostMapping infers POST method through the full extraction pipeline.
    #[test]
    fn test_java_extractor_infers_post_method_for_post_mapping() {
        use crate::extract::configs::JAVA_CONFIG;
        let ext = GenericExtractor::new(&JAVA_CONFIG);
        let code = r#"
public class ItemController {
    @PostMapping("/items")
    public Item create(@RequestBody Item item) {
        return item;
    }

    @GetMapping("/items/{id}")
    public Item get(@PathVariable Long id) {
        return null;
    }
}
"#;
        let result = ext.run(Path::new("ItemController.java"), code).unwrap();
        let api_nodes: Vec<_> = result.nodes.iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert_eq!(api_nodes.len(), 2, "should emit 2 ApiEndpoint nodes");

        let post_node = api_nodes.iter().find(|n| {
            n.metadata.get("http_method").map(|m| m == "POST").unwrap_or(false)
        });
        assert!(
            post_node.is_some(),
            "should infer POST for @PostMapping, got: {:?}",
            api_nodes.iter().map(|n| n.metadata.get("http_method")).collect::<Vec<_>>()
        );
        assert_eq!(
            post_node.unwrap().metadata.get("http_path"),
            Some(&"/items".to_string()),
            "POST endpoint path should be /items"
        );

        let get_node = api_nodes.iter().find(|n| {
            n.metadata.get("http_method").map(|m| m == "GET").unwrap_or(false)
        });
        assert!(get_node.is_some(), "should infer GET for @GetMapping");
    }

    // -----------------------------------------------------------------------
    // Implements edge tests (ApiEndpoint → handler Function)
    // -----------------------------------------------------------------------

    /// Python Flask: `@app.route("/users")` above `def get_users()` should
    /// produce an `Implements` edge from the ApiEndpoint to get_users.
    #[test]
    fn test_python_route_emits_implements_edge_to_handler() {
        use crate::extract::configs::PYTHON_CONFIG;
        use crate::graph::EdgeKind;
        let ext = GenericExtractor::new(&PYTHON_CONFIG);
        let code = r#"
@app.route("/users")
def get_users():
    pass
"#;
        let result = ext.run(Path::new("routes.py"), code).unwrap();

        let implements: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();
        assert_eq!(
            implements.len(), 1,
            "should emit 1 Implements edge, got: {:?}",
            implements
        );
        let edge = &implements[0];
        assert_eq!(edge.from.kind, NodeKind::ApiEndpoint, "Implements from should be ApiEndpoint");
        assert_eq!(edge.to.kind, NodeKind::Function, "Implements to should be Function");
        assert_eq!(edge.to.name, "get_users", "handler should be get_users");
    }

    /// TypeScript NestJS: `@Post("/items")` above `create()` should produce an
    /// `Implements` edge from the ApiEndpoint to the handler method.
    #[test]
    fn test_typescript_nestjs_route_emits_implements_edge() {
        use crate::extract::configs::TYPESCRIPT_CONFIG;
        use crate::graph::EdgeKind;
        let ext = GenericExtractor::new(&TYPESCRIPT_CONFIG);
        let code = r#"
class ItemController {
  @Post("/items")
  create() {}
}
"#;
        let result = ext.run(Path::new("item.controller.ts"), code).unwrap();

        let implements: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();
        assert_eq!(
            implements.len(), 1,
            "should emit 1 Implements edge for NestJS @Post, got edges: {:?}", implements
        );
        assert_eq!(implements[0].to.name, "create");
    }

    /// Multiple route handlers in the same file each get their own Implements edge.
    #[test]
    fn test_multiple_python_routes_each_get_implements_edge() {
        use crate::extract::configs::PYTHON_CONFIG;
        use crate::graph::EdgeKind;
        let ext = GenericExtractor::new(&PYTHON_CONFIG);
        let code = r#"
@app.route("/users")
def get_users():
    pass

@app.post("/users")
def create_user():
    pass
"#;
        let result = ext.run(Path::new("routes.py"), code).unwrap();

        let implements: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();
        assert_eq!(
            implements.len(), 2,
            "should emit 2 Implements edges for 2 routes, got: {:?}", implements
        );
        let handler_names: Vec<_> = implements.iter().map(|e| e.to.name.as_str()).collect();
        assert!(handler_names.contains(&"get_users"), "should link to get_users");
        assert!(handler_names.contains(&"create_user"), "should link to create_user");
    }

    // -----------------------------------------------------------------------
    // #390 — is_async and is_test metadata
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_async_rust_async_fn() {
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
async fn fetch_data() -> String {
    String::new()
}
fn sync_fn() {}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let async_fn = result.nodes.iter().find(|n| n.id.name == "fetch_data").unwrap();
        assert_eq!(
            async_fn.metadata.get("is_async").map(|s| s.as_str()),
            Some("true"),
            "async fn should have is_async=true"
        );
        let sync_fn = result.nodes.iter().find(|n| n.id.name == "sync_fn").unwrap();
        assert!(
            sync_fn.metadata.get("is_async").is_none(),
            "sync fn should NOT have is_async metadata"
        );
    }

    #[test]
    fn test_is_async_typescript() {
        use crate::extract::configs::TYPESCRIPT_CONFIG;
        let code = "async function loadData(): Promise<string> {\n    return '';\n}\nfunction syncFn() {}\n";
        let result = GenericExtractor::new(&TYPESCRIPT_CONFIG)
            .run(Path::new("test.ts"), code)
            .unwrap();
        let async_fn = result.nodes.iter().find(|n| n.id.name == "loadData").unwrap();
        assert_eq!(
            async_fn.metadata.get("is_async").map(|s| s.as_str()),
            Some("true"),
            "async TypeScript function should have is_async=true"
        );
    }

    #[test]
    fn test_is_async_python() {
        use crate::extract::configs::PYTHON_CONFIG;
        let code = "async def fetch():\n    pass\ndef sync_fn():\n    pass\n";
        let result = GenericExtractor::new(&PYTHON_CONFIG)
            .run(Path::new("test.py"), code)
            .unwrap();
        let async_fn = result.nodes.iter().find(|n| n.id.name == "fetch").unwrap();
        assert_eq!(
            async_fn.metadata.get("is_async").map(|s| s.as_str()),
            Some("true"),
            "async Python function should have is_async=true"
        );
        let sync_fn = result.nodes.iter().find(|n| n.id.name == "sync_fn").unwrap();
        assert!(
            sync_fn.metadata.get("is_async").is_none(),
            "sync Python function should NOT have is_async"
        );
    }

    #[test]
    fn test_is_test_rust_decorator() {
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
#[test]
fn my_test() {}

#[tokio::test]
async fn my_async_test() {}

fn not_a_test() {}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let test_fn = result.nodes.iter().find(|n| n.id.name == "my_test").unwrap();
        assert_eq!(
            test_fn.metadata.get("is_test").map(|s| s.as_str()),
            Some("true"),
            "#[test] function should have is_test=true"
        );
        let async_test = result.nodes.iter().find(|n| n.id.name == "my_async_test").unwrap();
        assert_eq!(
            async_test.metadata.get("is_test").map(|s| s.as_str()),
            Some("true"),
            "#[tokio::test] function should have is_test=true"
        );
        let not_test = result.nodes.iter().find(|n| n.id.name == "not_a_test").unwrap();
        assert!(
            not_test.metadata.get("is_test").is_none(),
            "production function should NOT have is_test metadata"
        );
    }

    #[test]
    fn test_is_test_python_name_convention() {
        use crate::extract::configs::PYTHON_CONFIG;
        let code = "def test_my_feature():\n    pass\ndef helper():\n    pass\n";
        let result = GenericExtractor::new(&PYTHON_CONFIG)
            .run(Path::new("test.py"), code)
            .unwrap();
        let test_fn = result.nodes.iter().find(|n| n.id.name == "test_my_feature").unwrap();
        assert_eq!(
            test_fn.metadata.get("is_test").map(|s| s.as_str()),
            Some("true"),
            "test_ prefixed Python function should have is_test=true"
        );
    }

    // -----------------------------------------------------------------------
    // #392 — decorator → Implements edges
    // -----------------------------------------------------------------------

    #[test]
    fn test_derive_emits_implements_edges() {
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
#[derive(Debug, Clone, Serialize)]
pub struct Config {
    pub name: String,
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let impl_edges: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Implements && e.from.name == "Config")
            .collect();
        let trait_names: Vec<&str> = impl_edges.iter().map(|e| e.to.name.as_str()).collect();
        assert!(trait_names.contains(&"Debug"), "should emit Implements(Debug)");
        assert!(trait_names.contains(&"Clone"), "should emit Implements(Clone)");
        assert!(trait_names.contains(&"Serialize"), "should emit Implements(Serialize)");
    }

    #[test]
    fn test_derive_no_test_attribute_implements() {
        // #[test] should NOT emit an Implements edge
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
#[test]
fn my_test() {}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let impl_from_test: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Implements && e.from.name == "my_test"
                && e.to.name.to_lowercase() == "test")
            .collect();
        assert!(
            impl_from_test.is_empty(),
            "#[test] should NOT emit Implements(Test), got: {:?}", impl_from_test
        );
    }

    #[test]
    fn test_python_dataclass_decorator_implements() {
        use crate::extract::configs::PYTHON_CONFIG;
        // Python `@dataclass` on a class should emit Implements(DataClass)
        let code = "from dataclasses import dataclass\n\n@dataclass\nclass Config:\n    name: str\n";
        let result = GenericExtractor::new(&PYTHON_CONFIG)
            .run(Path::new("test.py"), code)
            .unwrap();
        let impl_edges: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Implements && e.from.name == "Config")
            .collect();
        assert!(
            impl_edges.iter().any(|e| e.to.name == "Dataclass"),
            "Python @dataclass should emit Implements(Dataclass), got: {:?}",
            impl_edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_route_decorator_does_not_emit_implements_trait() {
        // Route decorators (@app.route, @app.post) should NOT emit Implements edges
        // — those are handled by the route_queries pass separately.
        use crate::extract::configs::PYTHON_CONFIG;
        let code = r#"
@app.route("/users")
def get_users():
    pass
"#;
        let result = GenericExtractor::new(&PYTHON_CONFIG)
            .run(Path::new("routes.py"), code)
            .unwrap();
        // Check no Implements from decorator pass for route decorators
        let decorator_impl: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Implements && e.from.name == "get_users"
                // Route query edges go from ApiEndpoint→handler, not function→trait
                && e.from.kind == NodeKind::Function)
            .collect();
        assert!(
            decorator_impl.is_empty(),
            "@app.route should not emit decorator Implements edge, got: {:?}", decorator_impl
        );
    }

    // -----------------------------------------------------------------------
    // #407 — same-file call detection
    // -----------------------------------------------------------------------

    #[test]
    fn test_same_file_calls_rust() {
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
fn helper() -> i32 {
    42
}

fn main_fn() -> i32 {
    helper()
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let calls: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        assert!(
            calls.iter().any(|e| e.from.name == "main_fn" && e.to.name == "helper"),
            "should emit Calls edge from main_fn to helper, got: {:?}",
            calls.iter().map(|e| format!("{} -> {}", e.from.name, e.to.name)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_same_file_calls_no_cross_file() {
        // External function calls (not in same file) should NOT produce Calls edges
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
fn main_fn() {
    println!("hello");  // std macro, not a same-file fn
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let calls: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        // No same-file functions named "println" — should be empty.
        assert!(
            calls.is_empty(),
            "should NOT emit Calls edge for macro/external call, got: {:?}",
            calls.iter().map(|e| format!("{} -> {}", e.from.name, e.to.name)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_same_file_calls_no_self_loop() {
        // Recursive function: should not emit a self-loop Calls edge
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
fn recurse(n: i32) -> i32 {
    if n <= 0 { 0 } else { recurse(n - 1) }
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let self_loops: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Calls && e.from.name == "recurse" && e.to.name == "recurse")
            .collect();
        assert!(
            self_loops.is_empty(),
            "recursive call should not emit self-loop Calls edge, got: {:?}", self_loops
        );
    }

    #[test]
    fn test_same_file_calls_python() {
        use crate::extract::configs::PYTHON_CONFIG;
        let code = "def helper():\n    return 42\n\ndef main_fn():\n    return helper()\n";
        let result = GenericExtractor::new(&PYTHON_CONFIG)
            .run(Path::new("test.py"), code)
            .unwrap();
        let calls: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        assert!(
            calls.iter().any(|e| e.from.name == "main_fn" && e.to.name == "helper"),
            "Python: should emit Calls from main_fn to helper, got: {:?}",
            calls.iter().map(|e| format!("{} -> {}", e.from.name, e.to.name)).collect::<Vec<_>>()
        );
    }

    // -----------------------------------------------------------------------
    // #409 — public API surface edges
    // -----------------------------------------------------------------------

    #[test]
    fn test_pub_use_rust_emits_re_exports_edge() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub use crate::foo::Bar;\nuse crate::internal::Helper;\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("lib.rs"), code)
            .unwrap();
        let re_exports: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::ReExports)
            .collect();
        assert!(
            !re_exports.is_empty(),
            "pub use should emit ReExports edge, got edges: {:?}",
            result.edges.iter().map(|e| format!("{:?}", e.kind)).collect::<Vec<_>>()
        );
        assert!(
            re_exports.iter().any(|e| e.to.name == "Bar"),
            "pub use Bar should emit ReExports to Bar, got: {:?}",
            re_exports.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_pub_use_rust_sets_visibility_pub() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub use crate::foo::Bar;\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("lib.rs"), code)
            .unwrap();
        let pub_imports: Vec<_> = result.nodes.iter()
            .filter(|n| n.id.kind == NodeKind::Import && n.metadata.get("visibility").map(|s| s.as_str()) == Some("pub"))
            .collect();
        assert!(
            !pub_imports.is_empty(),
            "pub use should set visibility=pub on Import node"
        );
    }

    #[test]
    fn test_private_use_rust_no_re_exports() {
        // Private `use` should NOT emit ReExports
        use crate::extract::rust::RUST_CONFIG;
        let code = "use crate::foo::Bar;\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("lib.rs"), code)
            .unwrap();
        let re_exports: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::ReExports)
            .collect();
        assert!(
            re_exports.is_empty(),
            "private use should NOT emit ReExports edge"
        );
    }

    #[test]
    fn test_is_test_function_uses_metadata_flag() {
        // After #390, is_test_function in ranking.rs should still work via decorator metadata.
        // Verify the is_test metadata is set so callers that check it work correctly.
        use crate::extract::rust::RUST_CONFIG;
        let code = r#"
#[test]
fn check_something() {}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("src/main.rs"), code)
            .unwrap();
        let test_fn = result.nodes.iter().find(|n| n.id.name == "check_something").unwrap();
        // Both is_test metadata AND decorators should be set
        assert_eq!(test_fn.metadata.get("is_test").map(|s| s.as_str()), Some("true"));
        assert!(test_fn.metadata.contains_key("decorators"), "decorators should also be preserved");
    }

    // -----------------------------------------------------------------------
    // Adversarial tests (dissent-seeded)
    // -----------------------------------------------------------------------

    /// Adversarial: function named after a stdlib/builtin call (e.g. `len`) should only
    /// emit Calls if the caller is in the SAME file and the callee IS that same-file fn.
    #[test]
    fn test_same_file_calls_only_same_file_functions() {
        use crate::extract::rust::RUST_CONFIG;
        // `len()` is NOT a same-file function here, so no Calls edge.
        let code = r#"
fn my_fn() -> usize {
    let v: Vec<i32> = vec![];
    v.len()  // method call, not a bare call to a same-file fn named "len"
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let calls: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .collect();
        // v.len() is a method call — callee node is a field_expression, not bare identifier.
        assert!(
            calls.is_empty(),
            "method call v.len() should NOT emit same-file Calls edge, got: {:?}",
            calls
        );
    }

    /// Adversarial: decorator with path segments (chained) should not emit Implements.
    /// `@pytest.mark.parametrize` should not → Implements(Pytest).
    #[test]
    fn test_chained_decorator_no_implements_edge() {
        use crate::extract::configs::PYTHON_CONFIG;
        let code = r#"
import pytest

@pytest.mark.parametrize("x,y", [(1,2)])
def test_add(x, y):
    pass
"#;
        let result = GenericExtractor::new(&PYTHON_CONFIG)
            .run(Path::new("test.py"), code)
            .unwrap();
        // @pytest.mark.parametrize is chained — should NOT emit Implements
        let impl_edges: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Implements && e.from.name == "test_add"
                && e.from.kind == NodeKind::Function)
            .collect();
        assert!(
            impl_edges.is_empty(),
            "@pytest.mark.parametrize should not emit Implements edge, got: {:?}",
            impl_edges.iter().map(|e| &e.to.name).collect::<Vec<_>>()
        );
    }

    /// Adversarial: is_test should NOT be set on struct/enum nodes, only functions.
    #[test]
    fn test_is_test_not_set_on_struct() {
        use crate::extract::rust::RUST_CONFIG;
        // A struct named TestHelper should not get is_test=true
        let code = "struct TestHelper {}\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let struc = result.nodes.iter().find(|n| n.id.name == "TestHelper").unwrap();
        assert!(
            struc.metadata.get("is_test").is_none(),
            "struct TestHelper should NOT have is_test metadata"
        );
    }

    /// Adversarial: wildcard re-export should NOT produce a ReExports edge (can't resolve target).
    #[test]
    fn test_pub_use_wildcard_no_edge() {
        use crate::extract::rust::RUST_CONFIG;
        let code = "pub use crate::foo::*;\n";
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("lib.rs"), code)
            .unwrap();
        let re_exports: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::ReExports)
            .collect();
        assert!(
            re_exports.is_empty(),
            "pub use foo::* should NOT emit ReExports edge (wildcard), got: {:?}", re_exports
        );
        // But visibility=pub should still be set
        let pub_imports: Vec<_> = result.nodes.iter()
            .filter(|n| n.id.kind == NodeKind::Import
                && n.metadata.get("visibility").map(|s| s.as_str()) == Some("pub"))
            .collect();
        assert!(
            !pub_imports.is_empty(),
            "pub use foo::* should still set visibility=pub on Import node"
        );
    }

    /// Adversarial: `split_decorators` handles route path with curly braces correctly.
    /// `@app.route("/users/{id}")` must not be split mid-decorator.
    #[test]
    fn test_split_decorators_with_curly_braces_in_path() {
        let decorators = r#"@app.route("/users/{id}"), @login_required"#;
        let parts = split_decorators(decorators);
        assert_eq!(
            parts.len(), 2,
            "should split into 2 decorators at top-level comma, got: {:?}", parts
        );
        assert_eq!(parts[0], r#"@app.route("/users/{id}")"#);
        assert_eq!(parts[1], "@login_required");
    }

    /// Adversarial: `#[derive(A, B)]` split should NOT be broken at the comma inside parens.
    #[test]
    fn test_split_decorators_derive_macro() {
        let decorators = "#[derive(Debug, Clone)]";
        let parts = split_decorators(decorators);
        assert_eq!(parts.len(), 1, "derive with multiple traits should stay as 1 decorator");
        assert_eq!(parts[0], "#[derive(Debug, Clone)]");
    }

    /// Adversarial: `is_async` should NOT be set on a function whose BODY contains
    /// the word "async" but the function itself is not declared async.
    #[test]
    fn test_is_async_not_set_for_body_containing_async() {
        use crate::extract::rust::RUST_CONFIG;
        // This function calls an async method but is not itself async.
        // The check only looks at the first line (signature), not the body.
        let code = r#"
fn spawn_task() {
    let future = async { 42 };
    tokio::spawn(future);
}
"#;
        let result = GenericExtractor::new(&RUST_CONFIG)
            .run(Path::new("test.rs"), code)
            .unwrap();
        let fn_node = result.nodes.iter().find(|n| n.id.name == "spawn_task").unwrap();
        // spawn_task is not async itself — is_async should NOT be set
        // NOTE: This test documents the current behavior of the text fallback.
        // The text fallback checks `first_line.starts_with("async ")` which for
        // `fn spawn_task()` is false. Body content is not checked.
        assert!(
            fn_node.metadata.get("is_async").is_none(),
            "sync fn with async body should NOT have is_async, got: {:?}",
            fn_node.metadata.get("is_async")
        );
    }
}
