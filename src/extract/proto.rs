//! Protobuf schema extractor (tree-sitter-driven).
//!
//! Parses `.proto` files using `tree-sitter-proto` to extract:
//! - `message` definitions (including nested) -> `NodeKind::ProtoMessage`
//! - Fields within messages (including `oneof` inner fields) ->
//!   `NodeKind::Other("proto_field")` + `EdgeKind::HasField`
//! - `service` definitions -> `NodeKind::Other("proto_service")`
//! - RPC methods -> `NodeKind::Function` + `EdgeKind::Defines` from the service
//!   and `EdgeKind::DependsOn` edges to the request/response message types
//! - `import` statements -> `NodeKind::Import` + `EdgeKind::DependsOn` to a `Module`
//! - Top-level `option` statements and `enum` values -> `NodeKind::Const`
//!
//! Uses tree-sitter for scope/comment/brace awareness — the previous line scanner
//! lost data on `oneof`, nested messages, single-line `{}` blocks, and braces
//! inside comments (issue #647 and siblings).
//!
//! See `iceberg_*` and `regression_*` tests for the behavioral contract.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

/// Protobuf schema extractor using tree-sitter.
pub struct ProtoExtractor;

impl Default for ProtoExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtoExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for ProtoExtractor {
    fn extensions(&self) -> &[&str] {
        &["proto"]
    }

    fn name(&self) -> &str {
        "protobuf"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_proto::LANGUAGE.into())?;

        let tree = match parser.parse(content, None) {
            Some(t) => t,
            None => return Ok(ExtractionResult::default()),
        };

        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let source = content.as_bytes();
        let root = tree.root_node();

        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            walk_top_level(child, path, source, &mut nodes, &mut edges);
        }

        Ok(ExtractionResult { nodes, edges })
    }
}

fn walk_top_level(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    match node.kind() {
        "import" => extract_import(node, path, source, nodes, edges),
        "message" => extract_message(node, path, source, nodes, edges),
        "service" => extract_service(node, path, source, nodes, edges),
        "enum" => extract_enum(node, path, source, nodes),
        "option" => {
            if let Some(c) = extract_option_const(node, path, source, /*top_level=*/ true) {
                nodes.push(c);
            }
        }
        _ => {} // syntax, package, edition, empty_statement, extend, etc.
    }
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

fn extract_import(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let path_node = match node.child_by_field_name("path") {
        Some(p) => p,
        None => return,
    };
    let import_path = strip_quotes(text_of(path_node, source));
    if import_path.is_empty() {
        return;
    }

    let line_start = node.start_position().row + 1;
    let line_end = node.end_position().row + 1;
    let body = text_of(node, source).to_string();

    let import_node_id = NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: format!("import \"{}\"", import_path),
        kind: NodeKind::Import,
    };
    nodes.push(Node {
        id: import_node_id.clone(),
        language: "protobuf".to_string(),
        line_start,
        line_end,
        signature: body.clone(),
        body: body.clone(),
        metadata: BTreeMap::new(),
        source: ExtractionSource::Schema,
    });

    let target_id = NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: import_path,
        kind: NodeKind::Module,
    };
    edges.push(Edge {
        from: import_node_id,
        to: target_id,
        kind: EdgeKind::DependsOn,
        source: ExtractionSource::Schema,
        confidence: Confidence::Detected,
    });
}

// ---------------------------------------------------------------------------
// message (recursive: nested messages + nested enums + oneof)
// ---------------------------------------------------------------------------

fn extract_message(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let name = match find_named_child(node, "message_name")
        .and_then(|n| find_named_child(n, "identifier"))
    {
        Some(n) => text_of(n, source).to_string(),
        None => return,
    };
    if name.is_empty() {
        return;
    }

    let line_start = node.start_position().row + 1;
    let line_end = node.end_position().row + 1;
    let body = text_of(node, source).to_string();

    let msg_node_id = NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: name.clone(),
        kind: NodeKind::ProtoMessage,
    };
    nodes.push(Node {
        id: msg_node_id.clone(),
        language: "protobuf".to_string(),
        line_start,
        line_end,
        signature: format!("message {}", name),
        body,
        metadata: BTreeMap::new(),
        source: ExtractionSource::Schema,
    });

    if let Some(message_body) = find_named_child(node, "message_body") {
        let mut cursor = message_body.walk();
        for child in message_body.named_children(&mut cursor) {
            match child.kind() {
                "field" => emit_field(child, path, source, &msg_node_id, nodes, edges),
                "oneof" => {
                    let mut oc = child.walk();
                    for inner in child.named_children(&mut oc) {
                        if inner.kind() == "oneof_field" {
                            emit_field(inner, path, source, &msg_node_id, nodes, edges);
                        }
                    }
                }
                "message" => extract_message(child, path, source, nodes, edges),
                "enum" => extract_enum(child, path, source, nodes),
                // map_field, group, extend, extensions, reserved, option,
                // empty_statement: not part of the existing extractor's contract.
                _ => {}
            }
        }
    }
}

/// Emit a `proto_field` node + `HasField` edge for either a `field` or
/// `oneof_field` tree-sitter node. Both share the same shape: `type`,
/// `identifier` (field name), `field_number`, optional `field_options`, and
/// (for `field` only) optional `repeated` / `optional` / `required` keyword
/// tokens that appear as anonymous children before the type.
fn emit_field(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    parent_id: &NodeId,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let type_text = match find_named_child(node, "type") {
        Some(t) => text_of(t, source).trim().to_string(),
        None => return,
    };
    let field_name = match find_named_child(node, "identifier") {
        Some(n) => text_of(n, source).to_string(),
        None => return,
    };
    if field_name.is_empty() || type_text.is_empty() {
        return;
    }

    // Detect label keyword (anonymous tokens) that precedes the type child.
    // For `oneof_field`, no label is permitted by the grammar.
    let label = field_label(node);
    let field_type = match label {
        Some(lbl) => format!("{} {}", lbl, type_text),
        None => type_text,
    };

    let line_start = node.start_position().row + 1;
    let line_end = node.end_position().row + 1;
    let body = text_of(node, source).to_string();

    let field_node_id = NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: field_name,
        kind: NodeKind::Other("proto_field".to_string()),
    };

    let mut metadata = BTreeMap::new();
    metadata.insert("field_type".to_string(), field_type);
    metadata.insert("parent_message".to_string(), parent_id.name.clone());

    nodes.push(Node {
        id: field_node_id.clone(),
        language: "protobuf".to_string(),
        line_start,
        line_end,
        signature: body.clone(),
        body,
        metadata,
        source: ExtractionSource::Schema,
    });

    edges.push(Edge {
        from: parent_id.clone(),
        to: field_node_id,
        kind: EdgeKind::HasField,
        source: ExtractionSource::Schema,
        confidence: Confidence::Detected,
    });
}

/// Returns "repeated", "optional", or "required" if one appears as an anonymous
/// keyword child on the field, else `None`. Looks at *all* children (named and
/// anonymous) since these labels are unnamed string tokens in the grammar.
fn field_label(node: tree_sitter::Node<'_>) -> Option<&'static str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "repeated" => return Some("repeated"),
            "optional" => return Some("optional"),
            "required" => return Some("required"),
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// service / rpc
// ---------------------------------------------------------------------------

fn extract_service(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let svc_name = match find_named_child(node, "service_name")
        .and_then(|n| find_named_child(n, "identifier"))
    {
        Some(n) => text_of(n, source).to_string(),
        None => return,
    };
    if svc_name.is_empty() {
        return;
    }

    let line_start = node.start_position().row + 1;
    let line_end = node.end_position().row + 1;
    let body = text_of(node, source).to_string();

    let svc_node_id = NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: svc_name.clone(),
        kind: NodeKind::Other("proto_service".to_string()),
    };
    nodes.push(Node {
        id: svc_node_id.clone(),
        language: "protobuf".to_string(),
        line_start,
        line_end,
        signature: format!("service {}", svc_name),
        body,
        metadata: BTreeMap::new(),
        source: ExtractionSource::Schema,
    });

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "rpc" {
            extract_rpc(child, path, source, &svc_node_id, nodes, edges);
        }
    }
}

fn extract_rpc(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    service_id: &NodeId,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let method_name = match find_named_child(node, "rpc_name")
        .and_then(|n| find_named_child(n, "identifier"))
    {
        Some(n) => text_of(n, source).to_string(),
        None => return,
    };

    // The two `message_or_enum_type` children are request and response, in
    // source order. Each contains identifier child(ren); we take the full
    // dotted path text as the type name to preserve qualifications.
    let mut types = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "message_or_enum_type" {
            types.push(text_of(child, source).trim().to_string());
        }
    }
    if types.len() < 2 || method_name.is_empty() {
        return;
    }
    let request_type = types[0].clone();
    let response_type = types[1].clone();

    let line_start = node.start_position().row + 1;
    let line_end = node.end_position().row + 1;
    let body = text_of(node, source).to_string();

    let method_node_id = NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: method_name,
        kind: NodeKind::Function,
    };

    let mut metadata = BTreeMap::new();
    metadata.insert("request_type".to_string(), request_type.clone());
    metadata.insert("response_type".to_string(), response_type.clone());
    metadata.insert("parent_service".to_string(), service_id.name.clone());

    nodes.push(Node {
        id: method_node_id.clone(),
        language: "protobuf".to_string(),
        line_start,
        line_end,
        signature: body.clone(),
        body,
        metadata,
        source: ExtractionSource::Schema,
    });

    edges.push(Edge {
        from: service_id.clone(),
        to: method_node_id.clone(),
        kind: EdgeKind::Defines,
        source: ExtractionSource::Schema,
        confidence: Confidence::Detected,
    });

    edges.push(Edge {
        from: method_node_id.clone(),
        to: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: request_type,
            kind: NodeKind::ProtoMessage,
        },
        kind: EdgeKind::DependsOn,
        source: ExtractionSource::Schema,
        confidence: Confidence::Detected,
    });

    edges.push(Edge {
        from: method_node_id,
        to: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: response_type,
            kind: NodeKind::ProtoMessage,
        },
        kind: EdgeKind::DependsOn,
        source: ExtractionSource::Schema,
        confidence: Confidence::Detected,
    });
}

// ---------------------------------------------------------------------------
// enum (values emitted as Const; enum itself has no node — preserves the
// pre-existing extractor's contract)
// ---------------------------------------------------------------------------

fn extract_enum(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    let enum_name = match find_named_child(node, "enum_name")
        .and_then(|n| find_named_child(n, "identifier"))
    {
        Some(n) => text_of(n, source).to_string(),
        None => return,
    };
    if enum_name.is_empty() {
        return;
    }

    let body_node = match find_named_child(node, "enum_body") {
        Some(b) => b,
        None => return,
    };

    let mut cursor = body_node.walk();
    for child in body_node.named_children(&mut cursor) {
        if child.kind() != "enum_field" {
            continue;
        }
        let ev_name = match find_named_child(child, "identifier") {
            Some(n) => text_of(n, source).to_string(),
            None => continue,
        };
        let ev_val = find_named_child(child, "int_lit")
            .map(|n| text_of(n, source).to_string())
            .unwrap_or_default();
        if ev_name.is_empty() {
            continue;
        }

        let mut metadata = BTreeMap::new();
        if !ev_val.is_empty() {
            metadata.insert("value".to_string(), ev_val);
        }
        metadata.insert("synthetic".to_string(), "false".to_string());

        let line = child.start_position().row + 1;
        let body_text = text_of(child, source).to_string();

        nodes.push(Node {
            id: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: format!("{}.{}", enum_name, ev_name),
                kind: NodeKind::Const,
            },
            language: "protobuf".to_string(),
            line_start: line,
            line_end: line,
            signature: body_text.clone(),
            body: body_text,
            metadata,
            source: ExtractionSource::Schema,
        });
    }
}

// ---------------------------------------------------------------------------
// option (top-level only; option statements inside messages/enums/etc. are
// not part of the pre-existing extractor's contract)
// ---------------------------------------------------------------------------

fn extract_option_const(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    top_level: bool,
) -> Option<Node> {
    if !top_level {
        return None;
    }

    // Option name comes from the private `_option_name` rule which projects
    // its content (an identifier or full_ident) up as a named child of the
    // option node. The constant value follows.
    let mut name: Option<String> = None;
    let mut value: Option<String> = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "identifier" | "full_ident" if name.is_none() => {
                name = Some(text_of(child, source).to_string());
            }
            "constant" if value.is_none() => {
                value = Some(strip_quotes(text_of(child, source)));
            }
            _ => {}
        }
    }
    let name = name?;
    if name.is_empty() {
        return None;
    }

    let line_start = node.start_position().row + 1;
    let line_end = node.end_position().row + 1;
    let body = text_of(node, source).to_string();

    let mut metadata = BTreeMap::new();
    if let Some(v) = value
        && !v.is_empty()
    {
        metadata.insert("value".to_string(), v);
    }
    metadata.insert("synthetic".to_string(), "false".to_string());

    Some(Node {
        id: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name,
            kind: NodeKind::Const,
        },
        language: "protobuf".to_string(),
        line_start,
        line_end,
        signature: body.clone(),
        body,
        metadata,
        source: ExtractionSource::Schema,
    })
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn find_named_child<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| c.kind() == kind)
}

fn text_of<'a>(node: tree_sitter::Node<'_>, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let first = bytes[0];
        let last = bytes[s.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_proto_messages() {
        let extractor = ProtoExtractor::new();
        let content = r#"
syntax = "proto3";

message SearchRequest {
  string query = 1;
  int32 page_number = 2;
  int32 result_per_page = 3;
}

message SearchResponse {
  repeated string results = 1;
}
"#;
        let result = extractor
            .extract(Path::new("api/search.proto"), content)
            .unwrap();

        let messages: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ProtoMessage)
            .collect();
        assert_eq!(messages.len(), 2, "Should find 2 messages");

        let names: Vec<&str> = messages.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"SearchRequest"));
        assert!(names.contains(&"SearchResponse"));

        // Check fields
        let fields: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("proto_field".to_string()))
            .collect();
        assert_eq!(fields.len(), 4, "Should find 4 fields total");

        // Check HasField edges
        let has_field_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::HasField)
            .collect();
        assert_eq!(has_field_edges.len(), 4);
    }

    #[test]
    fn test_extract_proto_service_and_rpcs() {
        let extractor = ProtoExtractor::new();
        let content = r#"
syntax = "proto3";

message SearchRequest {
  string query = 1;
}

message SearchResponse {
  repeated string results = 1;
}

service SearchService {
  rpc Search (SearchRequest) returns (SearchResponse);
  rpc StreamSearch (SearchRequest) returns (SearchResponse);
}
"#;
        let result = extractor
            .extract(Path::new("api/search.proto"), content)
            .unwrap();

        // Service node
        let services: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("proto_service".to_string()))
            .collect();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].id.name, "SearchService");

        // RPC methods as Function nodes
        let rpcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| {
                n.id.kind == NodeKind::Function && n.metadata.get("parent_service").is_some()
            })
            .collect();
        assert_eq!(rpcs.len(), 2, "Should find 2 RPC methods");

        // Check DependsOn edges from RPC to message types
        let rpc_deps: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn && e.to.kind == NodeKind::ProtoMessage)
            .collect();
        assert_eq!(rpc_deps.len(), 4, "2 RPCs * 2 message refs = 4 edges");
    }

    #[test]
    fn test_extract_proto_imports() {
        let extractor = ProtoExtractor::new();
        let content = r#"
syntax = "proto3";

import "google/protobuf/timestamp.proto";
import "common/types.proto";

message Event {
  string name = 1;
}
"#;
        let result = extractor
            .extract(Path::new("api/events.proto"), content)
            .unwrap();

        let imports: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Import)
            .collect();
        assert_eq!(imports.len(), 2);

        let dep_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn && e.to.kind == NodeKind::Module)
            .collect();
        assert_eq!(dep_edges.len(), 2);
    }

    #[test]
    fn test_proto_extractor_extensions() {
        let extractor = ProtoExtractor::new();
        assert_eq!(extractor.extensions(), &["proto"]);
        assert_eq!(extractor.name(), "protobuf");
    }

    #[test]
    fn test_proto_language_is_protobuf() {
        let extractor = ProtoExtractor::new();
        let content = "message Foo {\n  string bar = 1;\n}\n";
        let result = extractor.extract(Path::new("test.proto"), content).unwrap();
        assert_eq!(result.nodes[0].language, "protobuf");
    }

    // -----------------------------------------------------------------------
    // Iceberg / regression suite (issue #647 + siblings)
    //
    // The tree-sitter port turns these from documentation-of-bugs into
    // positive correctness tests. Names retain the `iceberg_*` prefix to
    // preserve traceability to the original failure modes; `_panics_today`
    // suffixes are kept as historical markers (the panics no longer happen).
    // -----------------------------------------------------------------------

    /// Originally panicked in `extract_message_fields` because the line scanner's
    /// `find_block_end` returned `start_line` for `message Empty {}`. Tree-sitter
    /// parses single-line bodies natively.
    #[test]
    fn iceberg_single_line_empty_message_panics_today() {
        let extractor = ProtoExtractor::new();
        let content = "message Empty {}\n";
        let result = extractor.extract(Path::new("empty.proto"), content).unwrap();
        let messages: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ProtoMessage)
            .collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id.name, "Empty");
        let fields: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("proto_field".to_string()))
            .collect();
        assert!(fields.is_empty());
    }

    /// Same shape, different test name — kept for parallelism with the iceberg
    /// suite's "extracts" assertions and to match the contract's 17-test count.
    #[test]
    fn iceberg_single_line_empty_message_extracts_message_no_fields() {
        let extractor = ProtoExtractor::new();
        let content = "message Empty {}\n";
        let result = extractor.extract(Path::new("empty.proto"), content).unwrap();
        let messages: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ProtoMessage)
            .collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id.name, "Empty");
        let fields: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("proto_field".to_string()))
            .collect();
        assert!(fields.is_empty());
    }

    /// Single-line empty enum: line scanner panicked on inline slice. The
    /// pre-existing extractor model doesn't emit a node for the enum itself,
    /// only for its values — an empty enum therefore yields no `Const` nodes.
    #[test]
    fn iceberg_single_line_empty_enum_panics_today() {
        let extractor = ProtoExtractor::new();
        let content = "enum E {}\n";
        let result = extractor.extract(Path::new("e.proto"), content).unwrap();
        let consts: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Const)
            .collect();
        assert!(consts.is_empty(), "empty enum has no values; got {:?}", consts);
    }

    /// Single-line empty service: line scanner panicked. Service node still
    /// emitted; RPC list is empty.
    #[test]
    fn iceberg_single_line_empty_service_panics_today() {
        let extractor = ProtoExtractor::new();
        let content = "service S {}\n";
        let result = extractor.extract(Path::new("s.proto"), content).unwrap();
        let services: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("proto_service".to_string()))
            .collect();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].id.name, "S");
        let rpcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert!(rpcs.is_empty());
    }

    /// Single-line non-empty service: same panic family. RPC must be extracted
    /// with correct request/response types in metadata.
    #[test]
    fn iceberg_single_line_service_with_rpc_panics_today() {
        let extractor = ProtoExtractor::new();
        let content = "service S { rpc Foo (Bar) returns (Baz); }\n";
        let result = extractor.extract(Path::new("s.proto"), content).unwrap();
        let services: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("proto_service".to_string()))
            .collect();
        assert_eq!(services.len(), 1);
        let rpcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert_eq!(rpcs.len(), 1);
        assert_eq!(rpcs[0].id.name, "Foo");
        assert_eq!(
            rpcs[0].metadata.get("request_type").map(|s| s.as_str()),
            Some("Bar"),
        );
        assert_eq!(
            rpcs[0].metadata.get("response_type").map(|s| s.as_str()),
            Some("Baz"),
        );
        assert_eq!(
            rpcs[0].metadata.get("parent_service").map(|s| s.as_str()),
            Some("S"),
        );
    }

    /// `oneof` block inner fields: the line scanner skipped any line starting
    /// with `oneof `, dropping inner fields. Tree-sitter port surfaces them as
    /// regular `proto_field` nodes attached to the enclosing message.
    #[test]
    fn iceberg_oneof_inner_fields_extracted() {
        let extractor = ProtoExtractor::new();
        let content = r#"message M {
  oneof choice {
    string a = 1;
    int32 b = 2;
  }
}
"#;
        let result = extractor.extract(Path::new("m.proto"), content).unwrap();
        let fields: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("proto_field".to_string()))
            .collect();
        let names: Vec<&str> = fields.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"a"), "oneof field `a` must be extracted; got {:?}", names);
        assert!(names.contains(&"b"), "oneof field `b` must be extracted; got {:?}", names);
    }

    /// Nested message: the line scanner advanced past the outer block and the
    /// helper skipped lines starting with `message `, dropping the inner type.
    /// Tree-sitter port recurses into `message_body` and emits both.
    #[test]
    fn iceberg_nested_message_extracted() {
        let extractor = ProtoExtractor::new();
        let content = r#"message Outer {
  string outer_field = 1;
  message Inner {
    string inner_field = 1;
  }
}
"#;
        let result = extractor.extract(Path::new("nested.proto"), content).unwrap();
        let messages: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ProtoMessage)
            .collect();
        let names: Vec<&str> = messages.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"Outer"));
        assert!(
            names.contains(&"Inner"),
            "nested message `Inner` must be extracted; got {:?}",
            names
        );
    }

    /// Comment containing `{`: the line scanner's depth tracker counted the
    /// brace inside the comment, miscounted block end, and lost downstream
    /// top-level messages. Tree-sitter ignores comments at the lexer level.
    #[test]
    fn iceberg_brace_in_comment_does_not_break_depth() {
        let extractor = ProtoExtractor::new();
        let content = r#"message M {
  // a comment with { brace
  string x = 1;
}
message N {
  string y = 1;
}
"#;
        let result = extractor.extract(Path::new("c.proto"), content).unwrap();
        let messages: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ProtoMessage)
            .collect();
        let names: Vec<&str> = messages.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"M"));
        assert!(
            names.contains(&"N"),
            "second message must be detected even when first message contains `{{` in a comment; got {:?}",
            names
        );
    }

    // --- Regression: working cases that must keep working through the port ---

    #[test]
    fn regression_empty_file() {
        let extractor = ProtoExtractor::new();
        let result = extractor.extract(Path::new("empty.proto"), "").unwrap();
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
    }

    #[test]
    fn regression_imports_only() {
        let extractor = ProtoExtractor::new();
        let content = "syntax = \"proto3\";\nimport \"a/b.proto\";\nimport \"c.proto\";\n";
        let result = extractor.extract(Path::new("i.proto"), content).unwrap();
        let imports: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Import)
            .collect();
        assert_eq!(imports.len(), 2);
    }

    #[test]
    fn regression_options_only() {
        let extractor = ProtoExtractor::new();
        let content = "syntax = \"proto3\";\noption go_package = \"example.com/foo\";\n";
        let result = extractor.extract(Path::new("o.proto"), content).unwrap();
        let consts: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Const)
            .collect();
        assert_eq!(consts.len(), 1);
        assert_eq!(consts[0].id.name, "go_package");
    }

    #[test]
    fn regression_multi_line_message_header() {
        // Brace on its own line — should not panic, message body extracted.
        let extractor = ProtoExtractor::new();
        let content = "message M\n{\n  string x = 1;\n}\n";
        let result = extractor.extract(Path::new("m.proto"), content).unwrap();
        let messages: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ProtoMessage)
            .collect();
        assert_eq!(messages.len(), 1, "expected M; got {:?}", messages);
    }
}
