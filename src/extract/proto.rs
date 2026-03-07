//! Protobuf schema extractor.
//!
//! Parses `.proto` files using line-based parsing to extract:
//! - `message` definitions -> `NodeKind::ProtoMessage` nodes
//! - Fields within messages -> `NodeKind::Other("proto_field")` nodes + `EdgeKind::HasField` edges
//! - `service` definitions -> `NodeKind::Other("proto_service")` nodes
//! - RPC methods -> `NodeKind::Function` nodes + edges to request/response message types
//! - `import` statements -> `EdgeKind::DependsOn` edges

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::{ExtractionResult, Extractor};

/// Protobuf schema extractor using line-based parsing.
pub struct ProtoExtractor;

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
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        let lines: Vec<&str> = content.lines().collect();
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i].trim();

            // import statements
            if line.starts_with("import ") {
                let import_path = line
                    .trim_start_matches("import ")
                    .trim_end_matches(';')
                    .trim()
                    .trim_matches('"')
                    .to_string();

                if !import_path.is_empty() {
                    let import_node_id = NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: format!("import \"{}\"", import_path),
                        kind: NodeKind::Import,
                    };
                    nodes.push(Node {
                        id: import_node_id.clone(),
                        language: "protobuf".to_string(),
                        line_start: i + 1,
                        line_end: i + 1,
                        signature: line.to_string(),
                        body: line.to_string(),
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
                i += 1;
                continue;
            }

            // message definitions
            if line.starts_with("message ") {
                let msg_name = line
                    .trim_start_matches("message ")
                    .trim_end_matches('{')
                    .trim()
                    .to_string();

                if !msg_name.is_empty() {
                    let block_start = i;
                    let block_end = find_block_end(&lines, i);
                    let body = lines[block_start..=block_end.min(lines.len() - 1)].join("\n");

                    let msg_node_id = NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: msg_name.clone(),
                        kind: NodeKind::ProtoMessage,
                    };
                    nodes.push(Node {
                        id: msg_node_id.clone(),
                        language: "protobuf".to_string(),
                        line_start: block_start + 1,
                        line_end: block_end + 1,
                        signature: format!("message {}", msg_name),
                        body: body.clone(),
                        metadata: BTreeMap::new(),
                        source: ExtractionSource::Schema,
                    });

                    // Extract fields within the message
                    extract_message_fields(
                        &lines,
                        block_start + 1,
                        block_end,
                        path,
                        &msg_node_id,
                        &mut nodes,
                        &mut edges,
                    );

                    i = block_end + 1;
                    continue;
                }
            }

            // service definitions
            if line.starts_with("service ") {
                let svc_name = line
                    .trim_start_matches("service ")
                    .trim_end_matches('{')
                    .trim()
                    .to_string();

                if !svc_name.is_empty() {
                    let block_start = i;
                    let block_end = find_block_end(&lines, i);
                    let body = lines[block_start..=block_end.min(lines.len() - 1)].join("\n");

                    let svc_node_id = NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: svc_name.clone(),
                        kind: NodeKind::Other("proto_service".to_string()),
                    };
                    nodes.push(Node {
                        id: svc_node_id.clone(),
                        language: "protobuf".to_string(),
                        line_start: block_start + 1,
                        line_end: block_end + 1,
                        signature: format!("service {}", svc_name),
                        body: body.clone(),
                        metadata: BTreeMap::new(),
                        source: ExtractionSource::Schema,
                    });

                    // Extract RPC methods
                    extract_rpc_methods(
                        &lines,
                        block_start + 1,
                        block_end,
                        path,
                        &svc_node_id,
                        &mut nodes,
                        &mut edges,
                    );

                    i = block_end + 1;
                    continue;
                }
            }

            i += 1;
        }

        Ok(ExtractionResult { nodes, edges })
    }
}

/// Find the closing brace for a block starting at `start_line`.
fn find_block_end(lines: &[&str], start_line: usize) -> usize {
    let mut depth = 0i32;
    for (j, line) in lines.iter().enumerate().skip(start_line) {
        for ch in line.chars() {
            if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
        }
    }
    lines.len().saturating_sub(1)
}

/// Extract fields from within a message block.
fn extract_message_fields(
    lines: &[&str],
    start: usize,
    end: usize,
    path: &Path,
    parent_id: &NodeId,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    for j in start..end {
        let field_line = lines[j].trim();
        // Skip empty lines, comments, closing braces, nested messages, options
        if field_line.is_empty()
            || field_line.starts_with("//")
            || field_line.starts_with('}')
            || field_line.starts_with("message ")
            || field_line.starts_with("enum ")
            || field_line.starts_with("oneof ")
            || field_line.starts_with("option ")
            || field_line.starts_with("reserved ")
            || field_line.starts_with("map<")
        {
            continue;
        }

        // Parse field: [repeated|optional] type name = number;
        if let Some((field_type, field_name)) = parse_proto_field(field_line) {
            let field_node_id = NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: field_name.clone(),
                kind: NodeKind::Other("proto_field".to_string()),
            };

            let mut metadata = BTreeMap::new();
            metadata.insert("field_type".to_string(), field_type);
            metadata.insert("parent_message".to_string(), parent_id.name.clone());

            nodes.push(Node {
                id: field_node_id.clone(),
                language: "protobuf".to_string(),
                line_start: j + 1,
                line_end: j + 1,
                signature: field_line.to_string(),
                body: field_line.to_string(),
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
    }
}

/// Parse a protobuf field line into (type, name).
/// Handles: `string query = 1;`, `repeated int32 ids = 2;`, `optional Foo bar = 3;`
fn parse_proto_field(line: &str) -> Option<(String, String)> {
    let line = line.trim().trim_end_matches(';').trim();
    // Remove field number: everything after the last `=`
    let line = if let Some(eq_pos) = line.rfind('=') {
        line[..eq_pos].trim()
    } else {
        line
    };

    let parts: Vec<&str> = line.split_whitespace().collect();
    match parts.len() {
        2 => Some((parts[0].to_string(), parts[1].to_string())),
        3 if parts[0] == "repeated" || parts[0] == "optional" || parts[0] == "required" => {
            Some((format!("{} {}", parts[0], parts[1]), parts[2].to_string()))
        }
        _ => None,
    }
}

/// Extract RPC methods from within a service block.
fn extract_rpc_methods(
    lines: &[&str],
    start: usize,
    end: usize,
    path: &Path,
    service_id: &NodeId,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    for j in start..end {
        let rpc_line = lines[j].trim();
        if !rpc_line.starts_with("rpc ") {
            continue;
        }

        if let Some((method_name, request_type, response_type)) = parse_rpc_line(rpc_line) {
            let method_node_id = NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: method_name.clone(),
                kind: NodeKind::Function,
            };

            let mut metadata = BTreeMap::new();
            metadata.insert("request_type".to_string(), request_type.clone());
            metadata.insert("response_type".to_string(), response_type.clone());
            metadata.insert("parent_service".to_string(), service_id.name.clone());

            nodes.push(Node {
                id: method_node_id.clone(),
                language: "protobuf".to_string(),
                line_start: j + 1,
                line_end: j + 1,
                signature: rpc_line.to_string(),
                body: rpc_line.to_string(),
                metadata,
                source: ExtractionSource::Schema,
            });

            // Edge from service to method
            edges.push(Edge {
                from: service_id.clone(),
                to: method_node_id.clone(),
                kind: EdgeKind::Defines,
                source: ExtractionSource::Schema,
                confidence: Confidence::Detected,
            });

            // Edge from method to request message type
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

            // Edge from method to response message type
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
    }
}

/// Parse an RPC line into (method_name, request_type, response_type).
/// Example: `rpc Search (SearchRequest) returns (SearchResponse);`
fn parse_rpc_line(line: &str) -> Option<(String, String, String)> {
    let line = line.trim_start_matches("rpc ").trim();

    // Extract method name (before first paren)
    let paren_pos = line.find('(')?;
    let method_name = line[..paren_pos].trim().to_string();

    // Extract request type: between first ( and first )
    let after_open = &line[paren_pos + 1..];
    let close_pos = after_open.find(')')?;
    let request_type = after_open[..close_pos].trim().to_string();

    // Extract response type: after "returns", between ( and )
    let returns_pos = line.find("returns")?;
    let after_returns = &line[returns_pos + 7..];
    let resp_open = after_returns.find('(')?;
    let resp_after = &after_returns[resp_open + 1..];
    let resp_close = resp_after.find(')')?;
    let response_type = resp_after[..resp_close].trim().to_string();

    if method_name.is_empty() || request_type.is_empty() || response_type.is_empty() {
        return None;
    }

    Some((method_name, request_type, response_type))
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
                n.id.kind == NodeKind::Function
                    && n.metadata.get("parent_service").is_some()
            })
            .collect();
        assert_eq!(rpcs.len(), 2, "Should find 2 RPC methods");

        // Check DependsOn edges from RPC to message types
        let rpc_deps: Vec<_> = result
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::DependsOn
                    && e.to.kind == NodeKind::ProtoMessage
            })
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
        let result = extractor
            .extract(Path::new("test.proto"), content)
            .unwrap();
        assert_eq!(result.nodes[0].language, "protobuf");
    }
}
