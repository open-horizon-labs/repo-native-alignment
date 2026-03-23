//! Post-extraction pass that emits `BelongsTo` edges from every node to a
//! virtual `NodeKind::Module` node derived from its directory path.
//!
//! # Problem
//!
//! RNA needs `BelongsTo` edges so agents can ask "show me everything in the
//! payments module" or walk the module hierarchy.  Previously these edges were
//! only emitted inside the LSP enricher (Pass 4).  The LSP enricher only runs
//! when rust-analyzer (or another LSP) reaches quiescent state — which never
//! happens on a fresh scan of a large repo.  The result: zero BelongsTo edges
//! for Rust unless LSP finished initialising.
//!
//! # Solution
//!
//! [`directory_module_pass`] runs as a post-extraction step alongside
//! [`api_link_pass`](super::api_link::api_link_pass) and
//! [`tested_by_pass`](super::naming_convention::tested_by_pass).  It only
//! needs the relative file paths stored on each node — no LSP required — so
//! it fires reliably on every scan, for every language.
//!
//! # Algorithm
//!
//! For every node that has a file path with at least one directory component:
//!
//! 1. Derive the immediate parent directory name (e.g. `src/extract/foo.rs`
//!    → `extract`).
//! 2. Create a virtual `NodeKind::Module` node for that directory, anchored to
//!    the same root and the directory path (e.g. `root:src/extract::extract:module`).
//! 3. Emit one `EdgeKind::BelongsTo` edge: symbol → module node.
//!
//! Nodes whose file is directly in the root (no parent directory) use the
//! file stem as the module name (`main.rs` → `main`).
//!
//! # Relationship with LSP Pass 4
//!
//! The LSP enricher's Pass 4 emits more accurate `BelongsTo` edges for Rust
//! by using `rust-analyzer/parentModule` (which understands `mod` declarations
//! and crate paths like `crate::server::graph`).  Those LSP edges are
//! preserved in the graph alongside the directory-derived edges; agents see
//! whichever fired first (or both if the dedup logic permits duplicate module
//! paths).  The intent is that this pass always provides a usable baseline and
//! LSP overrides with precision when available.
//!
//! # Placement
//!
//! Call this **after all nodes from all roots have been merged** (i.e., in
//! `build_full_graph_inner` and `update_graph_with_scan` alongside the
//! other post-extraction passes).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// Public return type
// ---------------------------------------------------------------------------

/// Nodes and edges emitted by the directory-module pass.
pub struct DirectoryModuleResult {
    /// Virtual `NodeKind::Module` nodes, one per unique directory.
    pub nodes: Vec<Node>,
    /// `BelongsTo` edges: symbol → module node.
    pub edges: Vec<Edge>,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Post-extraction pass: emit `BelongsTo` edges from every symbol node to a
/// directory-derived `NodeKind::Module` node.
///
/// This pass requires **no LSP** and fires on every scan, providing a reliable
/// baseline of module-hierarchy edges for all languages.
///
/// # Arguments
///
/// * `all_nodes` — the complete merged node list (all roots).
///
/// # Returns
///
/// A [`DirectoryModuleResult`] containing new virtual module nodes and the
/// edges linking symbols to them.  The caller must extend `all_nodes` with
/// `result.nodes` **and** `all_edges` with `result.edges`.
pub fn directory_module_pass(all_nodes: &[Node]) -> DirectoryModuleResult {
    // Map (root, dir_path, module_name) -> module Node so we emit each module node once.
    // The key MUST include module_name because root-level files (e.g. main.rs, build.rs)
    // all have an empty dir_path but different module names.  Using only (root, dir_path)
    // would collapse them into whichever module node was inserted first.
    let mut module_nodes: HashMap<(String, PathBuf, String), Node> = HashMap::new();
    let mut edges: Vec<Edge> = Vec::new();

    for node in all_nodes {
        // Skip nodes that should not have module edges.
        // Module nodes themselves — avoid self-loops.
        if node.id.kind == NodeKind::Module {
            continue;
        }
        // Virtual structural nodes (subsystems, frameworks, channels, events, diagnostics)
        // have no meaningful directory hierarchy and must not get module edges.
        if matches!(&node.id.kind, NodeKind::Other(s) if matches!(s.as_str(), "diagnostic" | "subsystem" | "framework" | "channel" | "event")) {
            continue;
        }
        // Skip virtual / external nodes that have no real file (empty path).
        if node.id.file.as_os_str().is_empty() {
            continue;
        }
        // Skip nodes whose root is "external" — they come from LSP and have
        // no meaningful directory structure relative to any workspace root.
        if node.id.root == "external" {
            continue;
        }

        // Derive module name and path from the node's relative file path.
        let (module_name, module_path) = derive_module(&node.id.file);

        let module_name = match module_name {
            Some(n) if !n.is_empty() => n,
            _ => continue, // cannot derive a name — skip
        };

        let key = (node.id.root.clone(), module_path.clone(), module_name.clone());

        // Create the module node if we haven't seen this directory yet.
        let module_node_id = module_nodes
            .entry(key)
            .or_insert_with(|| {
                let id = NodeId {
                    root: node.id.root.clone(),
                    file: module_path.clone(),
                    name: module_name.clone(),
                    kind: NodeKind::Module,
                };
                Node {
                    id,
                    language: node.language.clone(),
                    line_start: 0,
                    line_end: 0,
                    signature: format!("mod {}", module_name),
                    body: String::new(),
                    metadata: BTreeMap::new(),
                    source: ExtractionSource::TreeSitter,
                }
            })
            .id
            .clone();

        edges.push(Edge {
            from: node.id.clone(),
            to: module_node_id,
            kind: EdgeKind::BelongsTo,
            source: ExtractionSource::TreeSitter,
            confidence: Confidence::Detected,
        });
    }

    let nodes: Vec<Node> = module_nodes.into_values().collect();

    if !edges.is_empty() {
        tracing::info!(
            "Directory module pass: {} BelongsTo edge(s), {} module node(s)",
            edges.len(),
            nodes.len(),
        );
    }

    DirectoryModuleResult { nodes, edges }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Derive a `(module_name, module_path)` pair from a file's relative path.
///
/// | Input | module_name | module_path |
/// |-------|------------|-------------|
/// | `src/extract/foo.rs` | `extract` | `src/extract` |
/// | `src/main.rs` | `src` | `src` |
/// | `main.rs` | `main` | `` (empty) |
/// | `cmd/server/main.go` | `server` | `cmd/server` |
///
/// The `module_path` is `file.parent()` (or empty for root-level files).
/// The `module_name` is the directory name if a parent exists, otherwise the
/// file stem.
fn derive_module(file: &std::path::Path) -> (Option<String>, PathBuf) {
    let parent = file
        .parent()
        .filter(|p| !p.as_os_str().is_empty());

    let module_path = parent
        .map(|p| p.to_path_buf())
        .unwrap_or_default();

    let module_name = if let Some(p) = parent {
        // Use the immediate parent directory name
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    } else {
        // Root-level file: use the file stem (e.g. `main.rs` → `main`)
        file.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    };

    (module_name, module_path)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

    fn make_node(root: &str, file: &str, name: &str, kind: NodeKind) -> Node {
        Node {
            id: NodeId {
                root: root.into(),
                file: PathBuf::from(file),
                name: name.into(),
                kind,
            },
            language: "rust".into(),
            line_start: 1,
            line_end: 10,
            signature: format!("fn {}()", name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    // -------------------------------------------------------------------------
    // Basic happy-path tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_function_in_subdir_gets_belongs_to_edge() {
        let nodes = vec![
            make_node("repo", "src/payments.rs", "process_payment", NodeKind::Function),
        ];
        let result = directory_module_pass(&nodes);

        assert_eq!(result.edges.len(), 1, "expected one BelongsTo edge");
        assert_eq!(result.nodes.len(), 1, "expected one module node");

        let edge = &result.edges[0];
        assert_eq!(edge.kind, EdgeKind::BelongsTo);
        assert_eq!(edge.from.name, "process_payment");
        assert_eq!(edge.to.kind, NodeKind::Module);
        assert_eq!(edge.to.name, "src");
        assert_eq!(edge.source, ExtractionSource::TreeSitter);
    }

    #[test]
    fn test_nested_dir_uses_immediate_parent() {
        // src/extract/foo.rs → module "extract", path "src/extract"
        let nodes = vec![
            make_node("repo", "src/extract/foo.rs", "bar", NodeKind::Function),
        ];
        let result = directory_module_pass(&nodes);

        assert_eq!(result.edges.len(), 1);
        let edge = &result.edges[0];
        assert_eq!(edge.to.name, "extract");
        assert_eq!(edge.to.file, PathBuf::from("src/extract"));
    }

    #[test]
    fn test_root_level_file_uses_file_stem() {
        // main.rs at root → module "main"
        let nodes = vec![
            make_node("repo", "main.rs", "run", NodeKind::Function),
        ];
        let result = directory_module_pass(&nodes);

        assert_eq!(result.edges.len(), 1);
        let edge = &result.edges[0];
        assert_eq!(edge.to.name, "main");
        assert_eq!(edge.to.file, PathBuf::new());
    }

    #[test]
    fn test_multiple_nodes_same_dir_share_module_node() {
        let nodes = vec![
            make_node("repo", "src/lib.rs", "foo", NodeKind::Function),
            make_node("repo", "src/lib.rs", "bar", NodeKind::Struct),
            make_node("repo", "src/main.rs", "main", NodeKind::Function),
        ];
        let result = directory_module_pass(&nodes);

        // All three nodes are in "src/" → one module node, three edges
        assert_eq!(result.nodes.len(), 1, "all three share the same module node");
        assert_eq!(result.edges.len(), 3);
        for e in &result.edges {
            assert_eq!(e.to.name, "src");
        }
    }

    #[test]
    fn test_different_dirs_produce_separate_module_nodes() {
        let nodes = vec![
            make_node("repo", "src/handler.rs", "handle", NodeKind::Function),
            make_node("repo", "tests/integration.rs", "test_handle", NodeKind::Function),
        ];
        let result = directory_module_pass(&nodes);

        assert_eq!(result.nodes.len(), 2, "two different dirs → two module nodes");
        assert_eq!(result.edges.len(), 2);
        let names: std::collections::HashSet<&str> =
            result.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains("src"));
        assert!(names.contains("tests"));
    }

    #[test]
    fn test_module_nodes_are_skipped() {
        // A NodeKind::Module node must not produce a self-loop
        let mut module_node = make_node("repo", "src/mod.rs", "mymod", NodeKind::Function);
        module_node.id.kind = NodeKind::Module;

        let fn_node = make_node("repo", "src/lib.rs", "do_thing", NodeKind::Function);
        let nodes = vec![module_node, fn_node];

        let result = directory_module_pass(&nodes);

        // Only the fn_node should produce an edge
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].from.name, "do_thing");
    }

    #[test]
    fn test_external_root_nodes_are_skipped() {
        let node = make_node("external", "some/path.rs", "ext_fn", NodeKind::Function);
        let result = directory_module_pass(&[node]);
        assert!(result.edges.is_empty(), "external root nodes must be skipped");
    }

    #[test]
    fn test_empty_file_path_skipped() {
        let node = make_node("repo", "", "virtual_fn", NodeKind::Function);
        let result = directory_module_pass(&[node]);
        assert!(result.edges.is_empty(), "empty file path must be skipped");
    }

    #[test]
    fn test_diagnostic_nodes_are_skipped() {
        let mut diag = make_node("repo", "src/lib.rs", "E0001", NodeKind::Function);
        diag.id.kind = NodeKind::Other("diagnostic".into());
        let result = directory_module_pass(&[diag]);
        assert!(result.edges.is_empty(), "diagnostic nodes must be skipped");
    }

    #[test]
    fn test_edge_source_is_tree_sitter() {
        let nodes = vec![
            make_node("repo", "src/lib.rs", "foo", NodeKind::Function),
        ];
        let result = directory_module_pass(&nodes);
        assert_eq!(
            result.edges[0].source,
            ExtractionSource::TreeSitter,
            "source must be TreeSitter (not LSP) for the tree-sitter pass"
        );
    }

    #[test]
    fn test_empty_input_returns_empty() {
        let result = directory_module_pass(&[]);
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
    }

    // -------------------------------------------------------------------------
    // Adversarial tests
    // -------------------------------------------------------------------------

    #[test]
    fn adversarial_idempotent_on_repeated_call() {
        let nodes = vec![
            make_node("repo", "src/lib.rs", "foo", NodeKind::Function),
            make_node("repo", "src/main.rs", "main", NodeKind::Function),
        ];
        let result1 = directory_module_pass(&nodes);
        let result2 = directory_module_pass(&nodes);
        assert_eq!(result1.edges.len(), result2.edges.len(),
            "repeated calls must produce the same number of edges");
        assert_eq!(result1.nodes.len(), result2.nodes.len(),
            "repeated calls must produce the same number of module nodes");
    }

    #[test]
    fn adversarial_multiple_roots_no_cross_contamination() {
        // Two roots with the same file path should produce independent module nodes
        let nodes = vec![
            make_node("root-a", "src/lib.rs", "foo", NodeKind::Function),
            make_node("root-b", "src/lib.rs", "bar", NodeKind::Function),
        ];
        let result = directory_module_pass(&nodes);

        // Two module nodes: one per root
        assert_eq!(result.nodes.len(), 2, "each root gets its own module node");
        let roots: std::collections::HashSet<&str> =
            result.nodes.iter().map(|n| n.id.root.as_str()).collect();
        assert!(roots.contains("root-a"));
        assert!(roots.contains("root-b"));

        // Each edge points to its own root's module node
        for edge in &result.edges {
            assert_eq!(edge.from.root, edge.to.root,
                "BelongsTo edge must stay within the same root");
        }
    }

    #[test]
    fn adversarial_deep_nesting_uses_immediate_parent() {
        // a/b/c/d/file.rs → module name "d", path "a/b/c/d"
        let nodes = vec![
            make_node("repo", "a/b/c/d/file.rs", "deep_fn", NodeKind::Function),
        ];
        let result = directory_module_pass(&nodes);

        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].to.name, "d",
            "must use immediate parent dir name, not root-level");
        assert_eq!(result.edges[0].to.file, PathBuf::from("a/b/c/d"));
    }

    #[test]
    fn adversarial_non_function_nodes_also_get_edges() {
        // BelongsTo should apply to structs, traits, enums, etc. too
        let nodes = vec![
            make_node("repo", "src/types.rs", "MyStruct", NodeKind::Struct),
            make_node("repo", "src/types.rs", "MyTrait", NodeKind::Trait),
            make_node("repo", "src/types.rs", "MyEnum", NodeKind::Enum),
        ];
        let result = directory_module_pass(&nodes);

        assert_eq!(result.edges.len(), 3,
            "BelongsTo should apply to all symbol kinds, not just functions");
        assert_eq!(result.nodes.len(), 1, "all share the same module node");
    }

    // -------------------------------------------------------------------------
    // derive_module unit tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_derive_module_subdir() {
        let (name, path) = derive_module(std::path::Path::new("src/extract/foo.rs"));
        assert_eq!(name, Some("extract".to_string()));
        assert_eq!(path, PathBuf::from("src/extract"));
    }

    #[test]
    fn test_derive_module_top_level_dir() {
        let (name, path) = derive_module(std::path::Path::new("src/lib.rs"));
        assert_eq!(name, Some("src".to_string()));
        assert_eq!(path, PathBuf::from("src"));
    }

    #[test]
    fn test_derive_module_root_file() {
        let (name, path) = derive_module(std::path::Path::new("main.rs"));
        assert_eq!(name, Some("main".to_string()));
        assert_eq!(path, PathBuf::new());
    }

    #[test]
    fn test_derive_module_go_cmd() {
        let (name, path) = derive_module(std::path::Path::new("cmd/server/main.go"));
        assert_eq!(name, Some("server".to_string()));
        assert_eq!(path, PathBuf::from("cmd/server"));
    }

    // -------------------------------------------------------------------------
    // Regression tests (from CodeRabbit review)
    // -------------------------------------------------------------------------

    /// Regression: root-level files (main.rs, build.rs, lib.rs) all have
    /// empty `module_path` from `derive_module()`.  The dedup key must include
    /// `module_name` so each root-level file gets its OWN module node rather
    /// than being collapsed into whichever was inserted first.
    #[test]
    fn regression_root_level_files_get_distinct_module_nodes() {
        let nodes = vec![
            make_node("repo", "main.rs", "run", NodeKind::Function),
            make_node("repo", "build.rs", "main", NodeKind::Function),
            make_node("repo", "lib.rs", "lib_fn", NodeKind::Function),
        ];
        let result = directory_module_pass(&nodes);

        // Each root-level file must produce its OWN module node.
        assert_eq!(
            result.nodes.len(), 3,
            "main.rs, build.rs, lib.rs must each get a distinct module node, not share one"
        );

        let module_names: std::collections::HashSet<&str> =
            result.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(module_names.contains("main"), "main.rs → module 'main'");
        assert!(module_names.contains("build"), "build.rs → module 'build'");
        assert!(module_names.contains("lib"), "lib.rs → module 'lib'");

        // Each edge must point to the correct module for its source file.
        for edge in &result.edges {
            let src_stem = std::path::Path::new(edge.from.file.as_os_str())
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            assert_eq!(
                edge.to.name, src_stem,
                "symbol from {}.rs must BelongsTo module '{}'",
                src_stem, src_stem
            );
        }
    }
}
