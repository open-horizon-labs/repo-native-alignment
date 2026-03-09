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
                let target_name = parse_import_target(&name);
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
                            file: path.to_path_buf(),
                            name: target_name,
                            kind: NodeKind::Module,
                        },
                        kind: crate::graph::EdgeKind::DependsOn,
                        source: ExtractionSource::TreeSitter,
                        confidence: crate::graph::Confidence::Detected,
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

            // Scope propagation into children.
            if config.scope_parent_kinds.contains(&kind_str) {
                let scope = Some((name, node_kind));
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

/// Parse the target module name from an import declaration text.
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
    pub fn do_thing(&self) -> u32 {
        self.bar
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
        // Verify a Defines edge connects Foo (impl) -> do_thing
        assert!(
            defines_edges.iter().any(|e| e.to.name == "do_thing" && e.to.kind == NodeKind::Function),
            "Should have Defines edge to do_thing method"
        );
    }
}
