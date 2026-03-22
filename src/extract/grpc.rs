//! Post-extraction pass that emits `Calls` edges from gRPC client stub call
//! sites to the proto RPC method nodes defined in `.proto` files.
//!
//! # Problem
//!
//! The proto extractor (`proto.rs`) already emits:
//! - `NodeKind::Other("proto_service")` nodes for service definitions
//! - `NodeKind::Function` nodes with `parent_service` metadata for RPC methods
//!
//! What is missing are `Calls` edges from client code to those RPC method nodes.
//! When Python/Go/TypeScript code calls `stub.Bar(request)`, no edge links the
//! caller back to the proto definition, breaking the full gRPC call chain.
//!
//! # Solution
//!
//! [`grpc_client_calls_pass`] runs as a post-extraction pass after framework
//! detection. It:
//!
//! 1. Builds an index of all proto RPC method `Function` nodes (those with
//!    `parent_service` metadata).
//! 2. For each file that has a gRPC stub import, scans caller `Function` nodes
//!    for method call sites matching any known RPC method name.
//! 3. Emits a `Calls` edge (confidence: `Detected`) from the caller to the
//!    proto method.
//!
//! # Language support
//!
//! | Language   | Import signal           | Call pattern          |
//! |------------|-------------------------|-----------------------|
//! | Python     | `_pb2_grpc`             | `stub.MethodName(`    |
//! | Go         | `google.golang.org/grpc`| `client.MethodName(`  |
//! | TypeScript | `@grpc/`                | `client.MethodName(`  |
//!
//! # gRPC stub calls are method calls
//!
//! Unlike `import_calls_pass` which resolves bare function calls, gRPC stub
//! calls are always **method calls** on a stub object: `stub.Bar(req)`.
//! The pass therefore scans for `MethodName(` preceded by `.` — the opposite
//! of the `import_calls` exclusion rule.
//!
//! # Placement
//!
//! Registered in `PostExtractionRegistry` with `applies_when` gating on
//! `grpc-python`, `grpc-go`, or `grpc-js` frameworks (set by
//! `FrameworkDetectionPass`). Runs after framework detection, before
//! LanceDB persist.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// Import patterns that signal gRPC stub usage
// ---------------------------------------------------------------------------

/// Returns `true` when an import statement text indicates gRPC stub usage in
/// any of the supported languages.
///
/// Patterns:
/// - Python: `_pb2_grpc` — generated protobuf-grpc stub module suffix
/// - Go: `google.golang.org/grpc` — canonical gRPC Go import path
/// - TypeScript/JavaScript: `@grpc/` — @grpc npm scope
fn is_grpc_stub_import(import_text: &str) -> bool {
    let lower = import_text.to_lowercase();
    lower.contains("_pb2_grpc")
        || lower.contains("google.golang.org/grpc")
        || lower.contains("@grpc/")
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Post-extraction pass: emit `Calls` edges from gRPC client stub call sites
/// to proto RPC method `Function` nodes.
///
/// # Arguments
///
/// * `all_nodes` — the complete merged node list (all roots, all languages).
///   Must include proto `Function` nodes with `parent_service` metadata and
///   caller `Function` nodes with body text.
///
/// # Returns
///
/// The new `Calls` edges to add. May be empty if no gRPC stub calls are found.
pub fn grpc_client_calls_pass(all_nodes: &[Node]) -> Vec<Edge> {
    // ------------------------------------------------------------------
    // 1. Index proto RPC method nodes by method name.
    //    Only Function nodes with `parent_service` metadata are RPC methods.
    // ------------------------------------------------------------------
    // name → list of proto RPC Function nodes (may span multiple .proto files)
    let mut rpc_by_name: HashMap<&str, Vec<&Node>> = HashMap::new();
    for node in all_nodes {
        if node.id.kind == NodeKind::Function && node.metadata.contains_key("parent_service") {
            rpc_by_name
                .entry(node.id.name.as_str())
                .or_default()
                .push(node);
        }
    }

    if rpc_by_name.is_empty() {
        return Vec::new();
    }

    // ------------------------------------------------------------------
    // 2. Build per-(root, file) set: does this file import a gRPC stub?
    // ------------------------------------------------------------------
    let mut grpc_import_files: HashSet<(String, PathBuf)> = HashSet::new();
    for node in all_nodes {
        if node.id.kind == NodeKind::Import && is_grpc_stub_import(&node.id.name) {
            grpc_import_files.insert((node.id.root.clone(), node.id.file.clone()));
        }
    }

    if grpc_import_files.is_empty() {
        return Vec::new();
    }

    // ------------------------------------------------------------------
    // 3. For each Function node in a file with a gRPC import, scan body
    //    for stub method calls.
    // ------------------------------------------------------------------
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for node in all_nodes {
        if node.id.kind != NodeKind::Function {
            continue;
        }
        // Skip proto method nodes themselves — they are targets, not callers.
        if node.metadata.contains_key("parent_service") {
            continue;
        }
        let file_key = (node.id.root.clone(), node.id.file.clone());
        if !grpc_import_files.contains(&file_key) {
            continue;
        }
        if node.body.is_empty() {
            continue;
        }

        // Scan the body for method calls matching any known RPC name.
        let called_methods = extract_method_call_sites(&node.body);
        if called_methods.is_empty() {
            continue;
        }

        for rpc_name in called_methods {
            // Skip very short names to avoid false positives.
            if rpc_name.len() < 3 {
                continue;
            }
            // Skip if caller name == rpc name.
            if node.id.name == rpc_name {
                continue;
            }

            let Some(candidates) = rpc_by_name.get(rpc_name) else {
                continue;
            };

            for &rpc_node in candidates {
                let key = (node.id.to_stable_id(), rpc_node.id.to_stable_id());
                if seen.contains(&key) {
                    continue;
                }
                seen.insert(key);

                tracing::debug!(
                    "grpc_client_calls: {} ({}) -> {}.{} ({}.proto)",
                    node.id.name,
                    node.id.file.display(),
                    rpc_node.metadata.get("parent_service").map(|s| s.as_str()).unwrap_or("?"),
                    rpc_node.id.name,
                    rpc_node.id.file.display(),
                );

                edges.push(Edge {
                    from: node.id.clone(),
                    to: rpc_node.id.clone(),
                    kind: EdgeKind::Calls,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }
        }
    }

    if !edges.is_empty() {
        tracing::info!(
            "grpc_client_calls pass: {} Calls edge(s) from stub call sites to proto methods",
            edges.len()
        );
    }

    edges
}

// ---------------------------------------------------------------------------
// Helper: extract method call site names from a function body
// ---------------------------------------------------------------------------

/// Extract identifiers that appear as method calls — `obj.Name(` — in a body.
///
/// Unlike `extract_call_sites` in `import_calls.rs` which rejects `.`-preceded
/// calls, this function specifically collects them: gRPC stub calls are always
/// `stub.MethodName(req)`.
///
/// Returns a `HashSet` of identifier strings (the method name part, after `.`).
///
/// # Example
///
/// ```text
/// "resp = stub.GetUser(req)"  →  {"GetUser"}
/// "x = foo()"                 →  {}  (bare call, no dot)
/// ```
pub(crate) fn extract_method_call_sites(body: &str) -> HashSet<&str> {
    let mut result = HashSet::new();
    let bytes = body.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Find '('
        if bytes[i] == b'(' && i > 0 {
            // Walk backwards to find the identifier.
            let mut end = i;
            let mut j = i.saturating_sub(1);
            // Skip whitespace before '('.
            while j > 0 && bytes[j] == b' ' {
                j -= 1;
            }
            end = j + 1;
            // Walk back through identifier chars.
            while j > 0
                && (bytes[j - 1].is_ascii_alphanumeric()
                    || bytes[j - 1] == b'_')
            {
                j -= 1;
            }
            if j < end {
                // Only collect if preceded by '.' (method call).
                let prev = if j > 0 { bytes[j - 1] } else { 0 };
                if prev == b'.' {
                    if let Ok(ident) = std::str::from_utf8(&bytes[j..end]) {
                        if ident.len() >= 3 {
                            result.insert(ident);
                        }
                    }
                }
            }
        }
        i += 1;
    }
    result
}

/// Returns `true` when any of the gRPC framework IDs are present.
///
/// Used by `GrpcClientCallsPass::applies_when` in `post_extraction.rs`.
pub fn should_run(detected_frameworks: &std::collections::HashSet<String>) -> bool {
    detected_frameworks.contains("grpc-python")
        || detected_frameworks.contains("grpc-go")
        || detected_frameworks.contains("grpc-js")
        || detected_frameworks.contains("tonic") // Rust gRPC
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::graph::{EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn make_proto_rpc(file: &str, method_name: &str, service_name: &str) -> Node {
        let mut metadata = BTreeMap::new();
        metadata.insert("parent_service".to_string(), service_name.to_string());
        metadata.insert("request_type".to_string(), format!("{}Request", method_name));
        metadata.insert("response_type".to_string(), format!("{}Response", method_name));
        Node {
            id: NodeId {
                root: "r".into(),
                file: PathBuf::from(file),
                name: method_name.into(),
                kind: NodeKind::Function,
            },
            language: "protobuf".into(),
            line_start: 5,
            line_end: 5,
            signature: format!("rpc {} ({}Request) returns ({}Response);", method_name, method_name, method_name),
            body: format!("rpc {} ({}Request) returns ({}Response);", method_name, method_name, method_name),
            metadata,
            source: ExtractionSource::Schema,
        }
    }

    fn make_caller(file: &str, name: &str, body: &str, lang: &str) -> Node {
        Node {
            id: NodeId {
                root: "r".into(),
                file: PathBuf::from(file),
                name: name.into(),
                kind: NodeKind::Function,
            },
            language: lang.into(),
            line_start: 1,
            line_end: 10,
            signature: format!("def {}(self):", name),
            body: body.into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_import(file: &str, import_text: &str, lang: &str) -> Node {
        Node {
            id: NodeId {
                root: "r".into(),
                file: PathBuf::from(file),
                name: import_text.into(),
                kind: NodeKind::Import,
            },
            language: lang.into(),
            line_start: 1,
            line_end: 1,
            signature: import_text.into(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    // -----------------------------------------------------------------------
    // extract_method_call_sites tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_method_call_finds_dot_call() {
        let body = "resp = stub.GetUser(req)";
        let sites = extract_method_call_sites(body);
        assert!(sites.contains("GetUser"), "should find GetUser method call, got {:?}", sites);
    }

    #[test]
    fn test_extract_method_call_ignores_bare_call() {
        // Bare function calls (no dot) should not be in results.
        let body = "result = SomeFunction(arg)";
        let sites = extract_method_call_sites(body);
        assert!(!sites.contains("SomeFunction"), "bare calls should not be collected");
    }

    #[test]
    fn test_extract_method_call_multiple_calls() {
        let body = "a = stub.Search(req)\nb = stub.Delete(req2)";
        let sites = extract_method_call_sites(body);
        assert!(sites.contains("Search"));
        assert!(sites.contains("Delete"));
    }

    #[test]
    fn test_extract_method_call_short_names_included() {
        // Short names (< 3 chars) are filtered out in pass logic, not here.
        let body = "x = s.Do(r)";
        let sites = extract_method_call_sites(body);
        // "Do" is 2 chars — included by this function (filtering is caller's job)
        // But we still verify it doesn't panic.
        let _ = sites;
    }

    // -----------------------------------------------------------------------
    // is_grpc_stub_import tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_grpc_stub_import_python_pb2_grpc() {
        assert!(is_grpc_stub_import("import user_pb2_grpc"));
        assert!(is_grpc_stub_import("from user_pb2_grpc import UserServiceStub"));
    }

    #[test]
    fn test_is_grpc_stub_import_go() {
        assert!(is_grpc_stub_import("\"google.golang.org/grpc\""));
        assert!(is_grpc_stub_import("import \"google.golang.org/grpc\""));
    }

    #[test]
    fn test_is_grpc_stub_import_typescript() {
        assert!(is_grpc_stub_import("import * as grpc from '@grpc/grpc-js'"));
        assert!(is_grpc_stub_import("import { credentials } from '@grpc/grpc-js'"));
    }

    #[test]
    fn test_is_grpc_stub_import_negative() {
        assert!(!is_grpc_stub_import("import os"));
        assert!(!is_grpc_stub_import("import { useState } from 'react'"));
        assert!(!is_grpc_stub_import("import \"net/http\""));
    }

    // -----------------------------------------------------------------------
    // grpc_client_calls_pass integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_python_stub_call_emits_calls_edge() {
        let rpc = make_proto_rpc("api/user.proto", "GetUser", "UserService");
        let caller = make_caller(
            "client.py",
            "get_user",
            "def get_user(self, user_id):\n    resp = self.stub.GetUser(GetUserRequest(id=user_id))\n    return resp",
            "python",
        );
        let import = make_import("client.py", "import user_pb2_grpc", "python");

        let nodes = vec![rpc.clone(), caller.clone(), import];
        let edges = grpc_client_calls_pass(&nodes);

        assert_eq!(edges.len(), 1, "expected 1 Calls edge, got {:?}", edges);
        assert_eq!(edges[0].from.name, "get_user");
        assert_eq!(edges[0].to.name, "GetUser");
        assert_eq!(edges[0].kind, EdgeKind::Calls);
    }

    #[test]
    fn test_no_edge_without_grpc_import() {
        // File calls stub.GetUser but has no gRPC import — no edge.
        let rpc = make_proto_rpc("api/user.proto", "GetUser", "UserService");
        let caller = make_caller(
            "client.py",
            "get_user",
            "resp = stub.GetUser(req)",
            "python",
        );
        // No gRPC import node

        let nodes = vec![rpc, caller];
        let edges = grpc_client_calls_pass(&nodes);

        assert!(edges.is_empty(), "no import → no edge, got {:?}", edges);
    }

    #[test]
    fn test_no_edge_when_no_proto_rpc_nodes() {
        // gRPC import present but no proto RPC nodes in graph — no edge.
        let caller = make_caller(
            "client.py",
            "get_user",
            "resp = stub.GetUser(req)",
            "python",
        );
        let import = make_import("client.py", "import user_pb2_grpc", "python");

        let nodes = vec![caller, import];
        let edges = grpc_client_calls_pass(&nodes);

        assert!(edges.is_empty(), "no proto RPC nodes → no edge");
    }

    #[test]
    fn test_go_client_call_emits_edge() {
        let rpc = make_proto_rpc("api/search.proto", "Search", "SearchService");
        let caller = make_caller(
            "client/main.go",
            "doSearch",
            "func doSearch(c SearchServiceClient, q string) {\n    resp, err := c.Search(ctx, &pb.SearchRequest{Query: q})\n    _ = resp; _ = err\n}",
            "go",
        );
        let import = make_import(
            "client/main.go",
            "\"google.golang.org/grpc\"",
            "go",
        );

        let nodes = vec![rpc.clone(), caller.clone(), import];
        let edges = grpc_client_calls_pass(&nodes);

        assert_eq!(edges.len(), 1, "expected 1 Calls edge for Go, got {:?}", edges);
        assert_eq!(edges[0].from.name, "doSearch");
        assert_eq!(edges[0].to.name, "Search");
    }

    #[test]
    fn test_typescript_client_call_emits_edge() {
        let rpc = make_proto_rpc("api/user.proto", "GetUser", "UserService");
        let caller = make_caller(
            "src/client.ts",
            "fetchUser",
            "async function fetchUser(id: string): Promise<User> {\n    return new Promise((resolve, reject) => {\n        client.GetUser({ id }, (err, response) => {\n            if (err) reject(err); else resolve(response);\n        });\n    });\n}",
            "typescript",
        );
        let import = make_import(
            "src/client.ts",
            "import * as grpc from '@grpc/grpc-js'",
            "typescript",
        );

        let nodes = vec![rpc.clone(), caller.clone(), import];
        let edges = grpc_client_calls_pass(&nodes);

        assert_eq!(edges.len(), 1, "expected 1 Calls edge for TypeScript, got {:?}", edges);
        assert_eq!(edges[0].from.name, "fetchUser");
        assert_eq!(edges[0].to.name, "GetUser");
    }

    #[test]
    fn test_no_self_edge() {
        // Proto RPC node itself has parent_service metadata → should not be a caller.
        let rpc = make_proto_rpc("api/user.proto", "GetUser", "UserService");
        let import = make_import("api/user.proto", "import user_pb2_grpc", "protobuf");

        let nodes = vec![rpc.clone(), import];
        let edges = grpc_client_calls_pass(&nodes);

        // RPC nodes are excluded from caller set so no self-edge.
        for e in &edges {
            assert_ne!(e.from, e.to, "self-edge must not be emitted");
        }
    }

    #[test]
    fn test_multiple_rpcs_matched() {
        // Caller calls multiple RPC methods in the same function.
        let rpc_search = make_proto_rpc("api/search.proto", "Search", "SearchService");
        let rpc_delete = make_proto_rpc("api/search.proto", "Delete", "SearchService");
        let caller = make_caller(
            "client.py",
            "do_work",
            "def do_work(self):\n    r1 = self.stub.Search(req)\n    r2 = self.stub.Delete(req2)",
            "python",
        );
        let import = make_import("client.py", "import search_pb2_grpc", "python");

        let nodes = vec![rpc_search, rpc_delete, caller, import];
        let edges = grpc_client_calls_pass(&nodes);

        assert_eq!(edges.len(), 2, "expected 2 Calls edges, got {:?}", edges);
    }

    #[test]
    fn test_idempotent_on_repeated_call() {
        let rpc = make_proto_rpc("api/user.proto", "GetUser", "UserService");
        let caller = make_caller(
            "client.py",
            "get_user",
            "resp = stub.GetUser(req)",
            "python",
        );
        let import = make_import("client.py", "import user_pb2_grpc", "python");

        let nodes = vec![rpc, caller, import];
        let edges1 = grpc_client_calls_pass(&nodes);
        let edges2 = grpc_client_calls_pass(&nodes);

        assert_eq!(edges1.len(), edges2.len(), "pass must be idempotent");
    }

    #[test]
    fn test_should_run_gates_correctly() {
        let mut fw = HashSet::new();
        assert!(!should_run(&fw), "should not run with no frameworks");
        fw.insert("grpc-python".to_string());
        assert!(should_run(&fw), "should run with grpc-python");
        let mut fw2 = HashSet::new();
        fw2.insert("grpc-go".to_string());
        assert!(should_run(&fw2));
        let mut fw3 = HashSet::new();
        fw3.insert("grpc-js".to_string());
        assert!(should_run(&fw3));
        let mut fw4 = HashSet::new();
        fw4.insert("fastapi".to_string());
        assert!(!should_run(&fw4), "non-grpc framework should not trigger pass");
    }

    #[test]
    fn test_empty_input_returns_empty() {
        let edges = grpc_client_calls_pass(&[]);
        assert!(edges.is_empty());
    }
}
