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
        ("type_item",        NodeKind::TypeAlias),
        ("const_item",       NodeKind::Const),
        ("static_item",      NodeKind::Const),
        ("mod_item",         NodeKind::Module),
        ("use_declaration",  NodeKind::Import),
        ("macro_definition", NodeKind::Macro),
        ("field_declaration",        NodeKind::Field),
        ("enum_variant",             NodeKind::Field),
        ("function_signature_item",  NodeKind::Function),
    ],
    scope_parent_kinds: &["impl_item", "struct_item", "enum_item", "trait_item"],
    const_value_field: Some("value"),
    // use_declaration: the name IS the full `use crate::foo::Bar;` text.
    full_text_name_kinds: &["use_declaration"],
    string_literal_kinds: &[
        ("string_literal", Some("string_content")),
    ],
    param_container_field: Some("parameters"),
    param_type_field: Some("type"),
    return_type_field: Some("return_type"),
    type_requires_uppercase: true,
    branch_node_types: &[
        "if_expression", "else_clause",
        "match_expression", "match_arm",
        "for_expression", "while_expression", "loop_expression",
        "binary_expression",  // covers && and || (also arithmetic — same trade-off as Go/TS/Java)
        "try_expression",     // ?
    ],
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

        // Rust-specific: topology pattern detection (subprocess, network, async)
        // and static item metadata enrichment.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into())?;
        if let Some(tree) = parser.parse(content, None) {
            detect_topology_patterns(tree.root_node(), path, content.as_bytes(), &mut result.edges);
            enrich_static_metadata(tree.root_node(), content.as_bytes(), &mut result.nodes);
        }

        Ok(result)
    }
}


// ---------------------------------------------------------------------------
// Static item metadata enrichment
// ---------------------------------------------------------------------------

/// Walk the AST to find `static_item` nodes and enrich the corresponding
/// graph nodes (already emitted by the generic extractor as `Const`) with
/// `metadata["storage"] = "static"`. For `static mut`, also sets
/// `metadata["mutable"] = "true"`.
fn enrich_static_metadata(
    node: tree_sitter::Node,
    source: &[u8],
    nodes: &mut [crate::graph::Node],
) {
    if node.kind() == "static_item" {
        let line = node.start_position().row + 1;
        let name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok())
            .unwrap_or("");

        // Check for `mut` keyword among children
        let is_mutable = (0..node.child_count())
            .filter_map(|i| node.child(i as u32))
            .any(|c| c.kind() == "mutable_specifier");

        // Find the matching node emitted by the generic extractor
        for n in nodes.iter_mut() {
            if n.id.name == name
                && n.id.kind == NodeKind::Const
                && n.line_start == line
            {
                n.metadata
                    .insert("storage".to_string(), "static".to_string());
                if is_mutable {
                    n.metadata
                        .insert("mutable".to_string(), "true".to_string());
                }
                break;
            }
        }
    }

    // Recurse
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            enrich_static_metadata(child, source, nodes);
        }
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

    #[test]
    fn test_extract_rust_type_alias() {
        let extractor = RustExtractor::new();
        let code = r#"
pub type Result<T> = std::result::Result<T, MyError>;
type Callback = Box<dyn Fn(i32) -> bool>;
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let type_aliases: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::TypeAlias)
            .collect();
        assert_eq!(type_aliases.len(), 2, "Should find 2 type aliases");

        let names: Vec<&str> = type_aliases.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"Result"), "Should find type alias Result");
        assert!(names.contains(&"Callback"), "Should find type alias Callback");

        // Type aliases should be embeddable
        assert!(NodeKind::TypeAlias.is_embeddable(), "TypeAlias should be embeddable");
    }

    #[test]
    fn test_extract_rust_enum_variants() {
        let extractor = RustExtractor::new();
        let code = r#"
pub enum Status {
    Active,
    Inactive,
}

pub enum Color {
    Red,
    Green,
    Blue,
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let names: Vec<&str> = result.nodes.iter().map(|n| n.id.name.as_str()).collect();

        // Enum declarations should still be found
        assert!(names.contains(&"Status"), "Should find enum Status");
        assert!(names.contains(&"Color"), "Should find enum Color");

        // Enum variants should be indexed as Field nodes
        assert!(names.contains(&"Active"), "Should find variant Active");
        assert!(names.contains(&"Inactive"), "Should find variant Inactive");
        assert!(names.contains(&"Red"), "Should find variant Red");
        assert!(names.contains(&"Green"), "Should find variant Green");
        assert!(names.contains(&"Blue"), "Should find variant Blue");

        // Variants should have kind Field
        let active = result.nodes.iter().find(|n| n.id.name == "Active").unwrap();
        assert_eq!(active.id.kind, NodeKind::Field, "Variant should be Field kind");

        // Variants should have parent_scope pointing to the enum
        assert_eq!(
            active.metadata.get("parent_scope"),
            Some(&"Status".to_string()),
            "Variant should have parent_scope = Status"
        );

        let red = result.nodes.iter().find(|n| n.id.name == "Red").unwrap();
        assert_eq!(
            red.metadata.get("parent_scope"),
            Some(&"Color".to_string()),
            "Variant should have parent_scope = Color"
        );

        // Should produce HasField edges from enum to variant
        let has_field_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::HasField)
            .collect();
        assert!(
            has_field_edges.iter().any(|e| e.from.name == "Status" && e.to.name == "Active"),
            "Should have HasField edge Status -> Active"
        );
    }

    #[test]
    fn test_extract_rust_static_items() {
        let extractor = RustExtractor::new();
        let code = r#"
use std::sync::OnceLock;

static LOGGER: OnceLock<String> = OnceLock::new();
static mut COUNTER: u32 = 0;
pub static VERSION: &str = "1.0.0";
pub const MAX_SIZE: usize = 1024;
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let consts: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Const && n.metadata.get("synthetic").map(|s| s.as_str()) == Some("false"))
            .collect();
        let names: Vec<&str> = consts.iter().map(|n| n.id.name.as_str()).collect();

        // All four should be found as Const nodes
        assert!(names.contains(&"LOGGER"), "Should find static LOGGER, got: {:?}", names);
        assert!(names.contains(&"COUNTER"), "Should find static mut COUNTER, got: {:?}", names);
        assert!(names.contains(&"VERSION"), "Should find static VERSION, got: {:?}", names);
        assert!(names.contains(&"MAX_SIZE"), "Should find const MAX_SIZE, got: {:?}", names);

        // LOGGER should have storage=static, no mutable
        let logger = consts.iter().find(|n| n.id.name == "LOGGER").unwrap();
        assert_eq!(
            logger.metadata.get("storage").map(|s| s.as_str()),
            Some("static"),
            "LOGGER should have storage=static"
        );
        assert!(
            logger.metadata.get("mutable").is_none(),
            "LOGGER should not have mutable metadata"
        );

        // COUNTER should have storage=static AND mutable=true
        let counter = consts.iter().find(|n| n.id.name == "COUNTER").unwrap();
        assert_eq!(
            counter.metadata.get("storage").map(|s| s.as_str()),
            Some("static"),
            "COUNTER should have storage=static"
        );
        assert_eq!(
            counter.metadata.get("mutable").map(|s| s.as_str()),
            Some("true"),
            "COUNTER should have mutable=true"
        );

        // VERSION should have storage=static, value extracted
        let version = consts.iter().find(|n| n.id.name == "VERSION").unwrap();
        assert_eq!(
            version.metadata.get("storage").map(|s| s.as_str()),
            Some("static"),
            "VERSION should have storage=static"
        );

        // MAX_SIZE is a regular const, should NOT have storage metadata
        let max_size = consts.iter().find(|n| n.id.name == "MAX_SIZE").unwrap();
        assert!(
            max_size.metadata.get("storage").is_none(),
            "MAX_SIZE (const) should not have storage metadata"
        );
    }

    #[test]
    fn test_static_signature_contains_static_keyword() {
        let extractor = RustExtractor::new();
        let code = "static LOGGER: OnceLock<String> = OnceLock::new();\n";
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let logger = result
            .nodes
            .iter()
            .find(|n| n.id.name == "LOGGER")
            .expect("Should find LOGGER");
        assert!(
            logger.signature.contains("static"),
            "Static item signature should contain 'static', got: {}",
            logger.signature
        );
    }

    /// Adversarial: static with complex initializer should not extract a scalar value
    #[test]
    fn test_static_complex_initializer_no_value() {
        let extractor = RustExtractor::new();
        let code = r#"
static POOL: OnceLock<Vec<String>> = OnceLock::new();
static MUTEX: Mutex<HashMap<String, i32>> = Mutex::new(HashMap::new());
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let pool = result.nodes.iter().find(|n| n.id.name == "POOL").expect("Should find POOL");
        assert!(
            pool.metadata.get("value").is_none(),
            "Complex initializer should not have scalar value, got: {:?}",
            pool.metadata.get("value")
        );
        assert_eq!(pool.metadata.get("storage").map(|s| s.as_str()), Some("static"));

        let mutex = result.nodes.iter().find(|n| n.id.name == "MUTEX").expect("Should find MUTEX");
        assert!(mutex.metadata.get("value").is_none());
        assert_eq!(mutex.metadata.get("storage").map(|s| s.as_str()), Some("static"));
    }

    #[test]
    fn test_extract_rust_macro_definition() {
        let extractor = RustExtractor::new();
        let code = r#"
macro_rules! my_vec {
    ( $( $x:expr ),* ) => {
        {
            let mut v = Vec::new();
            $(
                v.push($x);
            )*
            v
        }
    };
}

macro_rules! simple_assert {
    ($cond:expr) => {
        if !$cond {
            panic!("assertion failed");
        }
    };
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        assert_eq!(macros.len(), 2, "Should find 2 macro definitions");

        let names: Vec<&str> = macros.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"my_vec"), "Should find macro my_vec");
        assert!(names.contains(&"simple_assert"), "Should find macro simple_assert");

        // Macros should be embeddable
        assert!(NodeKind::Macro.is_embeddable(), "Macro should be embeddable");

        // Macros should have language = rust
        assert_eq!(macros[0].language, "rust");
    }

    // -- Adversarial macro tests (seeded from dissent) --

    /// Adversarial: macro and function with same name in same file.
    /// NodeId is (root, file, name, kind) so these should not collide.
    #[test]
    fn test_rust_macro_and_fn_same_name_coexist() {
        let extractor = RustExtractor::new();
        let code = r#"
macro_rules! process {
    ($x:expr) => { $x + 1 };
}

fn process(x: i32) -> i32 {
    x + 1
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        let funcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function && n.id.name == "process")
            .collect();

        assert_eq!(macros.len(), 1, "Should find macro 'process'");
        assert_eq!(funcs.len(), 1, "Should find function 'process'");
        assert_ne!(macros[0].id.kind, funcs[0].id.kind, "Different NodeKinds");
    }

    /// Adversarial: macro with multiple match arms (complex body).
    #[test]
    fn test_rust_complex_macro_body_captured() {
        let extractor = RustExtractor::new();
        let code = r#"
macro_rules! count {
    () => { 0 };
    ($x:expr) => { 1 };
    ($x:expr, $($rest:expr),+) => { 1 + count!($($rest),+) };
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let macros: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Macro)
            .collect();
        assert_eq!(macros.len(), 1, "Should find complex macro");
        assert_eq!(macros[0].id.name, "count");
        // Body should contain the macro content (for embedding)
        assert!(!macros[0].body.is_empty(), "Macro body should be captured for embedding");
    }

    /// Adversarial: const and static with same name in same file (NodeId collision risk)
    #[test]
    fn test_const_and_static_same_name_both_extracted() {
        let extractor = RustExtractor::new();
        // This is unusual Rust code but syntactically valid
        let code = r#"
const FOO: u32 = 42;
static FOO: u32 = 42;
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        let foos: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.name == "FOO" && n.id.kind == NodeKind::Const
                && n.metadata.get("synthetic").map(|s| s.as_str()) == Some("false"))
            .collect();
        // Both should be extracted even though they'll have same NodeId
        // (this is the pre-existing #119 issue)
        assert!(
            foos.len() >= 2,
            "Should extract both const FOO and static FOO, got: {}",
            foos.len()
        );
        // At least one should have storage=static
        assert!(
            foos.iter().any(|n| n.metadata.get("storage").map(|s| s.as_str()) == Some("static")),
            "At least one FOO should have storage=static"
        );
        // At least one should NOT have storage metadata (the const)
        assert!(
            foos.iter().any(|n| n.metadata.get("storage").is_none()),
            "At least one FOO should be a plain const without storage"
        );
    }

    #[test]
    fn test_extract_rust_trait_method_signatures() {
        let extractor = RustExtractor::new();
        let code = r#"
pub trait Service {
    fn serve(&self);
    fn stop(&mut self);
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        // Trait itself should be found
        let service = result.nodes.iter().find(|n| n.id.name == "Service" && n.id.kind == NodeKind::Trait);
        assert!(service.is_some(), "Should find trait Service");

        // Trait methods should be indexed as Function nodes
        let serve = result.nodes.iter().find(|n| n.id.name == "serve" && n.id.kind == NodeKind::Function);
        assert!(serve.is_some(), "Should find trait method serve");

        let stop = result.nodes.iter().find(|n| n.id.name == "stop" && n.id.kind == NodeKind::Function);
        assert!(stop.is_some(), "Should find trait method stop");

        // Methods should have parent_scope pointing to the trait
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

        // Should produce Defines edges from trait to methods
        let defines_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Defines && e.from.name == "Service")
            .collect();
        assert!(
            defines_edges.iter().any(|e| e.to.name == "serve"),
            "Should have Defines edge Service -> serve"
        );
        assert!(
            defines_edges.iter().any(|e| e.to.name == "stop"),
            "Should have Defines edge Service -> stop"
        );
    }

    #[test]
    fn test_trait_default_method_and_signature_coexist() {
        let extractor = RustExtractor::new();
        let code = r#"
pub trait Handler {
    fn handle(&self, req: Request) -> Response;
    fn name(&self) -> &str {
        "default"
    }
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        // Both signature-only and default methods should be found
        let handle = result.nodes.iter().find(|n| n.id.name == "handle" && n.id.kind == NodeKind::Function);
        assert!(handle.is_some(), "Should find signature-only method handle");

        let name_fn = result.nodes.iter().find(|n| n.id.name == "name" && n.id.kind == NodeKind::Function);
        assert!(name_fn.is_some(), "Should find default method name");

        // Both should have parent_scope = Handler
        assert_eq!(handle.unwrap().metadata.get("parent_scope"), Some(&"Handler".to_string()));
        assert_eq!(name_fn.unwrap().metadata.get("parent_scope"), Some(&"Handler".to_string()));
    }

    #[test]
    fn test_no_duplicate_trait_and_impl_methods() {
        let extractor = RustExtractor::new();
        let code = r#"
pub trait Service {
    fn serve(&self);
}

struct MyService;

impl Service for MyService {
    fn serve(&self) {
        println!("serving");
    }
}
"#;
        let result = extractor.extract(Path::new("src/lib.rs"), code).unwrap();

        // There should be exactly 2 Function nodes named "serve":
        // one from the trait (function_signature_item) and one from the impl (function_item)
        let serves: Vec<_> = result.nodes.iter()
            .filter(|n| n.id.name == "serve" && n.id.kind == NodeKind::Function)
            .collect();
        assert_eq!(serves.len(), 2, "Should find exactly 2 serve methods (trait + impl), got: {}", serves.len());

        // One should have parent_scope = Service (trait), other = Service for MyService (impl)
        let trait_serve = serves.iter().find(|n| n.metadata.get("parent_scope") == Some(&"Service".to_string()));
        assert!(trait_serve.is_some(), "Should find trait serve with parent_scope=Service");

        let impl_serve = serves.iter().find(|n| n.metadata.get("parent_scope") == Some(&"Service for MyService".to_string()));
        assert!(impl_serve.is_some(), "Should find impl serve with parent_scope=Service for MyService");
    }
}
