//! Rust tree-sitter extractor.
//!
//! Extracts functions, structs, traits, enums, impls, consts, modules,
//! and use declarations from Rust source files. Also detects topology
//! patterns (subprocess spawn, network listeners, async boundaries).

use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, NodeId, NodeKind,
};

use super::generic::{GenericExtractor, LangConfig};
use super::{ExtractionResult, Extractor};

/// Static config for the generic traversal pass.
/// Topology detection and any Rust-specific logic run on top of this.
pub static RUST_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_rust::LANGUAGE.into(),
    language_name: "rust",
    extensions: &["rs"],
    node_kinds: &[
        ("function_item",    NodeKind::Function),
        ("struct_item",      NodeKind::Struct),
        ("trait_item",       NodeKind::Trait),
        ("impl_item",        NodeKind::Impl),
        ("enum_item",        NodeKind::Enum),
        ("const_item",       NodeKind::Const),
        ("mod_item",         NodeKind::Module),
        ("use_declaration",  NodeKind::Import),
        ("field_declaration",NodeKind::Field),
    ],
    scope_parent_kinds: &["impl_item", "struct_item", "enum_item"],
    const_value_field: Some("value"),
    // use_declaration: the name IS the full `use crate::foo::Bar;` text.
    full_text_name_kinds: &["use_declaration"],
    string_literal_kinds: &[
        ("string_literal", Some("string_content")),
    ],
    param_container_field: Some("parameters"),
    param_type_field: Some("type"),
    return_type_field: Some("return_type"),
};

/// Rust tree-sitter extractor with topology pattern detection.
pub struct RustExtractor {
    // Parser is not Send, so we create one per extract() call.
}

impl RustExtractor {
    pub fn new() -> Self {
        Self {}
    }
}

impl Extractor for RustExtractor {
    fn extensions(&self) -> &[&str] {
        &["rs"]
    }

    fn name(&self) -> &str {
        "rust-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        // Generic pass: function/struct/field/const/import/etc. + string literals.
        let mut result = GenericExtractor::new(&RUST_CONFIG).run(path, content)?;

        // Rust-specific: topology pattern detection (subprocess, network, async).
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            detect_topology_patterns(tree.root_node(), path, content.as_bytes(), &mut result.edges);
        }

        Ok(result)
    }
}


// ---------------------------------------------------------------------------
// Topology pattern detection
// ---------------------------------------------------------------------------

/// Detect common patterns that indicate runtime architecture.
///
/// Currently detects:
/// - `Command::new(...)` -> subprocess topology boundary
/// - `TcpListener::bind(...)` -> network listener topology boundary
/// - `tokio::spawn(...)` -> async boundary
fn detect_topology_patterns(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    edges: &mut Vec<Edge>,
) {
    let kind = node.kind();

    if kind == "call_expression" {
        if let Some(func_node) = node.child_by_field_name("function") {
            let func_text = func_node.utf8_text(source).unwrap_or("");

            // Find the enclosing function for context
            let enclosing = find_enclosing_function(node, source);
            let from_name = enclosing.unwrap_or_else(|| "<top-level>".to_string());

            if func_text == "Command::new" || func_text.ends_with("::Command::new") {
                let line = node.start_position().row + 1;
                edges.push(make_topology_edge(
                    path,
                    &from_name,
                    &format!("subprocess@L{}", line),
                    "subprocess",
                ));
            } else if func_text == "TcpListener::bind"
                || func_text.ends_with("::TcpListener::bind")
            {
                let line = node.start_position().row + 1;
                edges.push(make_topology_edge(
                    path,
                    &from_name,
                    &format!("tcp_listener@L{}", line),
                    "network_listener",
                ));
            } else if func_text == "tokio::spawn" {
                let line = node.start_position().row + 1;
                edges.push(make_topology_edge(
                    path,
                    &from_name,
                    &format!("async_task@L{}", line),
                    "async_boundary",
                ));
            }
        }
    }

    // Recurse
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            detect_topology_patterns(child, path, source, edges);
        }
    }
}

/// Walk up the AST to find the enclosing function name.
fn find_enclosing_function(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_item" {
            if let Some(name_node) = parent.child_by_field_name("name") {
                return name_node.utf8_text(source).ok().map(|s| s.to_string());
            }
        }
        current = parent.parent();
    }
    None
}

/// Create a topology boundary edge.
fn make_topology_edge(path: &Path, from_name: &str, to_name: &str, pattern: &str) -> Edge {
    Edge {
        from: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: from_name.to_string(),
            kind: NodeKind::Function,
        },
        to: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: to_name.to_string(),
            kind: NodeKind::Other(format!("topology:{}", pattern)),
        },
        kind: EdgeKind::TopologyBoundary,
        source: ExtractionSource::TreeSitter,
        confidence: Confidence::Detected,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_rust_functions_and_structs() {
        let extractor = RustExtractor::new();
        let code = r#"
pub fn hello(name: &str) -> String {
    format!("Hello, {}", name)
}

pub struct Config {
    pub port: u16,
}

pub trait Service {
    fn serve(&self);
}

pub enum Status {
    Active,
    Inactive,
}

pub const MAX_SIZE: usize = 1024;

mod inner {
    pub fn nested() {}
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let names: Vec<&str> = result.nodes.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"hello"), "Should find function hello");
        assert!(names.contains(&"Config"), "Should find struct Config");
        assert!(names.contains(&"Service"), "Should find trait Service");
        assert!(names.contains(&"Status"), "Should find enum Status");
        assert!(names.contains(&"MAX_SIZE"), "Should find const MAX_SIZE");
        assert!(names.contains(&"inner"), "Should find module inner");
    }

    #[test]
    fn test_rust_const_value_extraction() {
        let extractor = RustExtractor::new();
        let code = r#"
pub const MAX_RETRIES: u32 = 5;
pub const CONTENT_TYPE: &str = "application/json";
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();
        let consts: Vec<_> = result.nodes.iter().filter(|n| n.id.kind == NodeKind::Const).collect();
        // 2 declared consts + 1 synthetic string literal ("application/json" len > 3)
        assert!(consts.len() >= 2, "Should find at least 2 const nodes");

        let declared: Vec<_> = consts.iter().filter(|n| n.metadata.get("synthetic").map(|s| s.as_str()) == Some("false")).collect();
        assert_eq!(declared.len(), 2, "Should find exactly 2 declared (non-synthetic) const nodes");

        let max_retries = declared.iter().find(|n| n.id.name == "MAX_RETRIES").expect("Should find MAX_RETRIES");
        assert_eq!(max_retries.metadata.get("value").map(|s| s.as_str()), Some("5"), "Should extract value 5");
        assert_eq!(max_retries.metadata.get("synthetic").map(|s| s.as_str()), Some("false"));

        let content_type = declared.iter().find(|n| n.id.name == "CONTENT_TYPE").expect("Should find CONTENT_TYPE");
        assert!(content_type.metadata.get("value").is_some(), "Should extract string value");

        // The string literal value should also be captured as a synthetic Const
        let synthetic: Vec<_> = consts.iter().filter(|n| n.metadata.get("synthetic").map(|s| s.as_str()) == Some("true")).collect();
        assert!(!synthetic.is_empty(), "Should capture at least 1 synthetic string literal");
        assert!(synthetic.iter().any(|n| n.id.name == "application/json"), "Should capture 'application/json'");
    }

    #[test]
    fn test_extract_rust_imports() {
        let extractor = RustExtractor::new();
        let code = r#"
use std::path::Path;
use crate::graph::Node;
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let imports: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Import)
            .collect();
        assert_eq!(imports.len(), 2, "Should find 2 imports");

        // Should produce DependsOn edges
        let dep_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert_eq!(dep_edges.len(), 2, "Should produce 2 DependsOn edges");
    }

    #[test]
    fn test_extract_rust_impl_block() {
        let extractor = RustExtractor::new();
        let code = r#"
struct Foo;

impl Foo {
    pub fn method(&self) {}
}

impl Display for Foo {
    fn fmt(&self, f: &mut Formatter) -> Result {
        Ok(())
    }
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let impls: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Impl)
            .collect();
        assert_eq!(impls.len(), 2, "Should find 2 impl blocks");

        // Methods inside impl blocks should have parent_scope
        let method = result
            .nodes
            .iter()
            .find(|n| n.id.name == "method")
            .expect("Should find method");
        assert_eq!(
            method.metadata.get("parent_scope"),
            Some(&"Foo".to_string())
        );
    }

    #[test]
    fn test_topology_command_new() {
        let extractor = RustExtractor::new();
        let code = r#"
use std::process::Command;

fn run_child() {
    let output = Command::new("ls").output().unwrap();
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let topo_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::TopologyBoundary)
            .collect();
        assert!(
            !topo_edges.is_empty(),
            "Should detect Command::new topology boundary"
        );
        assert!(topo_edges[0].to.name.contains("subprocess"));
    }

    #[test]
    fn test_topology_tcp_listener() {
        let extractor = RustExtractor::new();
        let code = r#"
use std::net::TcpListener;

fn start_server() {
    let listener = TcpListener::bind("127.0.0.1:8080").unwrap();
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let topo_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::TopologyBoundary)
            .collect();
        assert!(
            !topo_edges.is_empty(),
            "Should detect TcpListener::bind topology boundary"
        );
        assert!(topo_edges[0].to.name.contains("tcp_listener"));
    }

    #[test]
    fn test_topology_tokio_spawn() {
        let extractor = RustExtractor::new();
        let code = r#"
async fn orchestrate() {
    tokio::spawn(async {
        do_work().await;
    });
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let topo_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::TopologyBoundary)
            .collect();
        assert!(
            !topo_edges.is_empty(),
            "Should detect tokio::spawn topology boundary"
        );
        assert!(topo_edges[0].to.name.contains("async_task"));
    }

    #[test]
    fn test_node_language_is_rust() {
        let extractor = RustExtractor::new();
        let code = "pub fn hello() {}\n";
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();
        assert_eq!(result.nodes[0].language, "rust");
    }
}
