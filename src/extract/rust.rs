//! Rust tree-sitter extractor.
//!
//! Extracts functions, structs, traits, enums, impls, consts, modules,
//! and use declarations from Rust source files. Also detects topology
//! patterns (subprocess spawn, network listeners, async boundaries).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::{ExtractionResult, Extractor};

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
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
        let tree = parser
            .parse(content, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse {}", path.display()))?;

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let source = content.as_bytes();

        collect_nodes(
            tree.root_node(),
            path,
            source,
            &None,
            &mut nodes,
            &mut edges,
        );

        // Topology pattern detection
        detect_topology_patterns(tree.root_node(), path, source, &mut edges);

        Ok(ExtractionResult { nodes, edges })
    }
}

/// Map tree-sitter node kind string to our NodeKind.
fn ts_kind_to_node_kind(kind_str: &str) -> Option<NodeKind> {
    match kind_str {
        "function_item" => Some(NodeKind::Function),
        "struct_item" => Some(NodeKind::Struct),
        "trait_item" => Some(NodeKind::Trait),
        "impl_item" => Some(NodeKind::Impl),
        "enum_item" => Some(NodeKind::Enum),
        "const_item" => Some(NodeKind::Const),
        "mod_item" => Some(NodeKind::Module),
        "use_declaration" => Some(NodeKind::Import),
        _ => None,
    }
}

/// Recursively collect nodes from the tree-sitter AST.
fn collect_nodes(
    node: tree_sitter::Node,
    path: &Path,
    source: &[u8],
    parent_scope: &Option<String>,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let kind_str = node.kind();

    if let Some(node_kind) = ts_kind_to_node_kind(kind_str) {
        let name = extract_name(&node, &node_kind, source);
        let body = node.utf8_text(source).unwrap_or("").to_string();
        let signature = extract_signature(&body);
        let line_start = node.start_position().row + 1;
        let line_end = node.end_position().row + 1;

        let mut metadata = BTreeMap::new();
        if let Some(scope) = parent_scope {
            metadata.insert("parent_scope".to_string(), scope.clone());
        }
        // Store the byte column of the name identifier from the AST so that
        // the LSP enricher can position the cursor accurately without having
        // to search the signature string (which is fragile for overloaded
        // parameter names and multi-keyword prefixes).
        if let Some(name_node) = node.child_by_field_name("name") {
            metadata.insert(
                "name_col".to_string(),
                name_node.start_position().column.to_string(),
            );
        }

        let graph_node = Node {
            id: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: name.clone(),
                kind: node_kind.clone(),
            },
            language: "rust".to_string(),
            line_start,
            line_end,
            signature,
            body,
            metadata,
            source: ExtractionSource::TreeSitter,
        };

        // For import nodes, produce a DependsOn edge
        if node_kind == NodeKind::Import {
            let import_target = parse_use_target(&name);
            if !import_target.is_empty() {
                let target_id = NodeId {
                    root: String::new(),
                    file: path.to_path_buf(),
                    name: import_target,
                    kind: NodeKind::Module,
                };
                edges.push(Edge {
                    from: graph_node.id.clone(),
                    to: target_id,
                    kind: EdgeKind::DependsOn,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }
        }

        nodes.push(graph_node);

        // For impl blocks, recurse with the impl target as parent scope
        if node_kind == NodeKind::Impl {
            let scope = Some(name);
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    collect_nodes(child, path, source, &scope, nodes, edges);
                }
            }
            return;
        }
    }

    // Recurse into children
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_nodes(child, path, source, parent_scope, nodes, edges);
        }
    }
}

/// Extract the symbol name from a tree-sitter node.
fn extract_name(node: &tree_sitter::Node, kind: &NodeKind, source: &[u8]) -> String {
    match kind {
        NodeKind::Impl => {
            let trait_name = node
                .child_by_field_name("trait")
                .and_then(|n| n.utf8_text(source).ok())
                .map(|s| s.to_string());
            let type_name = node
                .child_by_field_name("type")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("unknown")
                .to_string();
            match trait_name {
                Some(t) => format!("{} for {}", t, type_name),
                None => type_name,
            }
        }
        NodeKind::Import => node
            .utf8_text(source)
            .unwrap_or("unknown")
            .to_string()
            .trim()
            .to_string(),
        _ => {
            if let Some(name_node) = node.child_by_field_name("name") {
                name_node
                    .utf8_text(source)
                    .unwrap_or("unknown")
                    .to_string()
            } else {
                "unknown".to_string()
            }
        }
    }
}

/// Extract the signature: text before the first `{`, or the first line.
fn extract_signature(body: &str) -> String {
    if let Some(brace_pos) = body.find('{') {
        let sig = body[..brace_pos].trim();
        if !sig.is_empty() {
            return sig.to_string();
        }
    }
    body.lines().next().unwrap_or("").trim().to_string()
}

/// Parse the target module/crate from a `use` declaration.
/// e.g., "use std::path::Path;" -> "std::path::Path"
fn parse_use_target(use_text: &str) -> String {
    use_text
        .trim_start_matches("use ")
        .trim_start_matches("pub use ")
        .trim_end_matches(';')
        .trim()
        .to_string()
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
