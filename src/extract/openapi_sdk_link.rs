//! Post-extraction pass: link generated SDK functions to their OpenAPI spec operations.
//!
//! # Problem
//!
//! After extraction, two node types represent the same HTTP operation but have no edge
//! between them:
//!
//! - **`NodeKind::Function`** in generated SDK files (e.g. `sdk.gen.ts`) — function names
//!   match the OpenAPI `operationId` (e.g. `listUsers`, `createUser`).
//! - **`NodeKind::ApiEndpoint`** in OpenAPI spec files — nodes with `operation_id` in
//!   their metadata (e.g. `GET /users` with `operation_id: listUsers`).
//!
//! Without this link, graph traversal from a TypeScript SDK call cannot reach the FastAPI
//! handler. The full chain is:
//!
//! ```text
//! TS SDK function  →[Implements]→  ApiEndpoint  →[Implements]→  FastAPI handler
//! ```
//!
//! This pass closes the first hop.
//!
//! # Matching strategy
//!
//! SDK function names are matched against `operation_id` metadata on `ApiEndpoint` nodes.
//! Names are normalised with [`normalize_operation_id`] so that camelCase (`listUsers`),
//! snake_case (`list_users`), and PascalCase (`ListUsers`) all compare equal. This handles
//! the common case where Python SDKs use snake_case but the spec uses camelCase.
//!
//! # Generated file detection
//!
//! Only function nodes in files whose name contains `sdk.gen`, `.generated.`, `_generated.`
//! (any extension — covers `.ts`, `.js`, `.py`, `.go`, `.kt`, etc.), `generated_client`,
//! or `openapi_client` are considered. This avoids false positives from non-generated
//! functions that happen to share a name with an operation.
//!
//! # Edge direction
//!
//! `Implements` edge from SDK Function → ApiEndpoint. The SDK function is the generated
//! implementation of the spec operation — analogous to how LSP emits `Implements` from
//! a concrete type to its supertype. (`api_link.rs` uses `DependsOn` for URL-literal →
//! ApiEndpoint links, which is a weaker, reference-only relationship.)

use std::collections::HashMap;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// Name normalisation
// ---------------------------------------------------------------------------

/// Normalise an operation identifier so that camelCase, snake_case, and
/// PascalCase variants compare equal.
///
/// Algorithm: strip underscores, lowercase everything.
///
/// | Input         | Output       |
/// |---------------|--------------|
/// | `listUsers`   | `listusers`  |
/// | `list_users`  | `listusers`  |
/// | `ListUsers`   | `listusers`  |
/// | `LIST_USERS`  | `listusers`  |
pub fn normalize_operation_id(id: &str) -> String {
    id.replace('_', "").to_lowercase()
}

// ---------------------------------------------------------------------------
// Generated-file detection
// ---------------------------------------------------------------------------

/// Return `true` if the file path looks like a generated SDK file.
///
/// Matches common code-gen output filename patterns:
/// - `sdk.gen.ts` / `sdk.gen.js` / `sdk.gen.py` (any `sdk.gen.*`)
/// - `api.generated.ts` / `client.generated.ts` (contains `.generated.`)
/// - `api_generated.py` / `client_generated.ts` / `_generated.go` (contains `_generated.`)
/// - `generated_client.ts` / `generated_client.py`
/// - `openapi_client.ts` / `openapi_client.py`
fn is_generated_sdk_file(path: &std::path::Path) -> bool {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    file_name.contains("sdk.gen")
        || file_name.contains(".generated.")
        || file_name.contains("_generated.")
        || file_name.contains("generated_client")
        || file_name.contains("openapi_client")
}

// ---------------------------------------------------------------------------
// Pass
// ---------------------------------------------------------------------------

/// Post-extraction pass: emit `Implements` edges from generated SDK functions
/// to their matching `ApiEndpoint` nodes.
///
/// Call this **after all nodes have been merged** across roots. The SDK file and
/// the OpenAPI spec are typically in different roots (or at least different files);
/// running this pass per-delta would miss cross-file links.
///
/// Complexity: O(E + F) where E is the number of `ApiEndpoint` nodes with an
/// `operation_id` and F is the number of Function nodes in generated SDK files —
/// both are typically small.
pub fn openapi_sdk_link_pass(all_nodes: &[Node]) -> Vec<Edge> {
    // Phase 1: build a map from normalised operation_id → Vec<NodeId> for all
    // ApiEndpoint nodes that carry an `operation_id` in their metadata.
    let mut endpoint_by_op_id: HashMap<String, Vec<NodeId>> = HashMap::new();
    for node in all_nodes {
        if node.id.kind != NodeKind::ApiEndpoint {
            continue;
        }
        if let Some(op_id) = node.metadata.get("operation_id") {
            if !op_id.is_empty() {
                let key = normalize_operation_id(op_id);
                endpoint_by_op_id.entry(key).or_default().push(node.id.clone());
            }
        }
    }

    if endpoint_by_op_id.is_empty() {
        return Vec::new();
    }

    // Phase 2: scan Function nodes in generated SDK files and match against the map.
    let mut edges = Vec::new();
    for node in all_nodes {
        if node.id.kind != NodeKind::Function {
            continue;
        }
        if !is_generated_sdk_file(&node.id.file) {
            continue;
        }

        let key = normalize_operation_id(&node.id.name);
        if let Some(ep_ids) = endpoint_by_op_id.get(&key) {
            for ep_id in ep_ids {
                edges.push(Edge {
                    from: node.id.clone(),
                    to: ep_id.clone(),
                    kind: EdgeKind::Implements,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }
        }
    }

    edges
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ExtractionSource, NodeId};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_sdk_fn(name: &str, file: &str) -> Node {
        Node {
            id: NodeId {
                root: "frontend".to_string(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind: NodeKind::Function,
            },
            language: "typescript".to_string(),
            line_start: 1,
            line_end: 5,
            signature: format!("export async function {}(", name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_api_endpoint(name: &str, op_id: &str, file: &str) -> Node {
        let mut metadata = BTreeMap::new();
        metadata.insert("operation_id".to_string(), op_id.to_string());
        Node {
            id: NodeId {
                root: "backend".to_string(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind: NodeKind::ApiEndpoint,
            },
            language: "openapi".to_string(),
            line_start: 0,
            line_end: 0,
            signature: name.to_string(),
            body: String::new(),
            metadata,
            source: ExtractionSource::Schema,
        }
    }

    // --- normalize_operation_id ---

    #[test]
    fn test_normalize_camel_case() {
        assert_eq!(normalize_operation_id("listUsers"), "listusers");
    }

    #[test]
    fn test_normalize_snake_case() {
        assert_eq!(normalize_operation_id("list_users"), "listusers");
    }

    #[test]
    fn test_normalize_pascal_case() {
        assert_eq!(normalize_operation_id("ListUsers"), "listusers");
    }

    #[test]
    fn test_normalize_screaming_snake_case() {
        assert_eq!(normalize_operation_id("LIST_USERS"), "listusers");
    }

    // --- is_generated_sdk_file ---

    #[test]
    fn test_sdk_gen_ts_detected() {
        assert!(is_generated_sdk_file(&PathBuf::from("src/api/sdk.gen.ts")));
    }

    #[test]
    fn test_generated_dot_ts_detected() {
        assert!(is_generated_sdk_file(&PathBuf::from("src/api/client.generated.ts")));
    }

    #[test]
    fn test_underscore_generated_ts_detected() {
        assert!(is_generated_sdk_file(&PathBuf::from("src/_generated.ts")));
    }

    #[test]
    fn test_underscore_generated_py_detected() {
        assert!(is_generated_sdk_file(&PathBuf::from("src/api_generated.py")));
    }

    #[test]
    fn test_underscore_generated_go_detected() {
        assert!(is_generated_sdk_file(&PathBuf::from("src/client_generated.go")));
    }

    #[test]
    fn test_generated_client_ts_detected() {
        assert!(is_generated_sdk_file(&PathBuf::from("src/generated_client.ts")));
    }

    #[test]
    fn test_openapi_client_detected() {
        assert!(is_generated_sdk_file(&PathBuf::from("src/openapi_client.ts")));
    }

    #[test]
    fn test_regular_service_not_detected() {
        assert!(!is_generated_sdk_file(&PathBuf::from("src/services/user_service.ts")));
    }

    #[test]
    fn test_plain_ts_not_detected() {
        assert!(!is_generated_sdk_file(&PathBuf::from("src/api/users.ts")));
    }

    // --- openapi_sdk_link_pass ---

    #[test]
    fn test_exact_camel_match_emits_implements_edge() {
        let nodes = vec![
            make_sdk_fn("listUsers", "src/api/sdk.gen.ts"),
            make_api_endpoint("GET /users", "listUsers", "openapi/api.yaml"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert_eq!(edges.len(), 1);
        let e = &edges[0];
        assert_eq!(e.kind, EdgeKind::Implements);
        assert_eq!(e.from.name, "listUsers");
        assert_eq!(e.to.name, "GET /users");
    }

    #[test]
    fn test_snake_case_sdk_matches_camel_op_id() {
        // Python-generated SDK uses snake_case; spec uses camelCase
        let nodes = vec![
            make_sdk_fn("list_users", "src/client/generated_client.py"),
            make_api_endpoint("GET /users", "listUsers", "openapi/api.yaml"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert_eq!(edges.len(), 1, "snake_case SDK fn should match camelCase op_id");
    }

    #[test]
    fn test_non_sdk_file_does_not_link() {
        // Function in a non-generated file must NOT link even if name matches
        let nodes = vec![
            make_sdk_fn("listUsers", "src/services/user_service.ts"),
            make_api_endpoint("GET /users", "listUsers", "openapi/api.yaml"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert!(
            edges.is_empty(),
            "function in non-generated file should produce no edge, got: {:?}", edges
        );
    }

    #[test]
    fn test_endpoint_without_operation_id_not_linked() {
        // ApiEndpoint with no operation_id metadata must not be matched
        let ep_no_op_id = {
            let mut n = make_api_endpoint("GET /users", "", "openapi/api.yaml");
            n.metadata.remove("operation_id");
            n
        };
        let nodes = vec![
            make_sdk_fn("listUsers", "src/api/sdk.gen.ts"),
            ep_no_op_id,
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert!(edges.is_empty(), "endpoint without operation_id must not match");
    }

    #[test]
    fn test_empty_operation_id_not_linked() {
        let nodes = vec![
            make_sdk_fn("listUsers", "src/api/sdk.gen.ts"),
            make_api_endpoint("GET /users", "", "openapi/api.yaml"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert!(edges.is_empty(), "empty operation_id must not produce edge");
    }

    #[test]
    fn test_no_endpoints_no_edges() {
        let nodes = vec![make_sdk_fn("listUsers", "src/api/sdk.gen.ts")];
        let edges = openapi_sdk_link_pass(&nodes);
        assert!(edges.is_empty());
    }

    #[test]
    fn test_no_sdk_functions_no_edges() {
        let nodes = vec![make_api_endpoint("GET /users", "listUsers", "openapi/api.yaml")];
        let edges = openapi_sdk_link_pass(&nodes);
        assert!(edges.is_empty());
    }

    #[test]
    fn test_multiple_sdk_fns_link_independently() {
        let nodes = vec![
            make_sdk_fn("listUsers", "src/api/sdk.gen.ts"),
            make_sdk_fn("createUser", "src/api/sdk.gen.ts"),
            make_api_endpoint("GET /users", "listUsers", "openapi/api.yaml"),
            make_api_endpoint("POST /users", "createUser", "openapi/api.yaml"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert_eq!(edges.len(), 2);
        let names: Vec<_> = edges.iter().map(|e| e.from.name.as_str()).collect();
        assert!(names.contains(&"listUsers"));
        assert!(names.contains(&"createUser"));
    }

    #[test]
    fn test_cross_root_linking() {
        // SDK in frontend root, spec in backend root — must still link
        let nodes = vec![
            make_sdk_fn("getUser", "apps/frontend/src/api/sdk.gen.ts"),
            make_api_endpoint("GET /users/{id}", "getUser", "apps/backend/openapi/api.yaml"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert_eq!(edges.len(), 1, "cross-root link must work");
        assert_eq!(edges[0].from.name, "getUser");
        assert_eq!(edges[0].to.name, "GET /users/{id}");
    }

    /// Adversarial: a function with the same name in both a generated and a non-generated
    /// file — only the generated one should be linked.
    #[test]
    fn test_only_generated_file_linked_when_both_present() {
        let nodes = vec![
            make_sdk_fn("listUsers", "src/api/sdk.gen.ts"),          // generated
            make_sdk_fn("listUsers", "src/services/user_service.ts"), // not generated
            make_api_endpoint("GET /users", "listUsers", "openapi/api.yaml"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert_eq!(edges.len(), 1, "only the generated file should produce an edge");
        assert_eq!(
            edges[0].from.file,
            PathBuf::from("src/api/sdk.gen.ts"),
            "edge should be from the generated SDK file"
        );
    }

    /// Adversarial: function name that partially matches (e.g. `listUser` vs `listUsers`)
    /// must NOT produce a false edge.
    #[test]
    fn test_partial_name_match_does_not_link() {
        let nodes = vec![
            make_sdk_fn("listUser", "src/api/sdk.gen.ts"),
            make_api_endpoint("GET /users", "listUsers", "openapi/api.yaml"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert!(edges.is_empty(), "partial name match must not produce edge");
    }
}
