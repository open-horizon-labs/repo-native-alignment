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
//! # Matching strategies
//!
//! ## Strategy 1: operation_id (primary)
//!
//! SDK function names are matched against `operation_id` metadata on `ApiEndpoint` nodes.
//! Names are normalised with [`normalize_operation_id`] so that camelCase (`listUsers`),
//! snake_case (`list_users`), and PascalCase (`ListUsers`) all compare equal. This handles
//! the common case where Python SDKs use snake_case but the spec uses camelCase.
//!
//! Works when:
//! - A checked-in OpenAPI YAML/JSON file is present in the repo, OR
//! - The SDK generator embeds `operationId` into function names.
//!
//! ## Strategy 2: URL path co-location (fallback)
//!
//! Many SDK generators (e.g. `@hey-api/openapi-ts`) embed the full server URL as a string
//! literal inside each generated function body:
//!
//! ```typescript
//! export const getWorkspacesIdExpertunities = (...) =>
//!   client.get({ url: '/workspaces/{id}/expertunities', ...options });
//! ```
//!
//! `string_literals.rs` already extracts these as synthetic `Const` nodes whose `name` is
//! the URL path.  When these `Const` nodes appear on the **same line** as a Function node
//! in the same generated SDK file, the function is the SDK call for that URL.
//!
//! This pass builds a secondary map from `(file, line) → Vec<normalized_path>` using those
//! Const nodes, then for each unmatched SDK Function, looks up any URL path on the same line
//! and matches it against an `ApiEndpoint` whose `http_path` normalises to the same string.
//!
//! Works when no OpenAPI spec file is checked in but the SDK embeds the URL inline —
//! the pattern used by `@hey-api/openapi-ts`, `openapi-fetch`, and similar generators.
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
use std::path::PathBuf;

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
// URL path normalisation (reuses logic from api_link.rs)
// ---------------------------------------------------------------------------

/// Normalise a URL path so that path-parameter styles compare equal.
///
/// | Input              | Output               |
/// |--------------------|----------------------|
/// | `/users/:id`       | `/users/<param>`     |
/// | `/users/{id}`      | `/users/<param>`     |
/// | `/users/[id]`      | `/users/<param>`     |
/// | `/workspaces/{id}` | `/workspaces/<param>`|
fn normalize_url_path(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            if seg.starts_with(':')
                || (seg.starts_with('{') && seg.ends_with('}'))
                || (seg.starts_with('[') && seg.ends_with(']'))
            {
                "<param>"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

// ---------------------------------------------------------------------------
// HTTP method inference from SDK function name
// ---------------------------------------------------------------------------

/// Infer the HTTP method from a generated SDK function name.
///
/// SDK generators like `@hey-api/openapi-ts` prefix function names with the
/// HTTP verb in camelCase: `getUsers` → `GET`, `postWorkspaces` → `POST`, etc.
///
/// | Input                            | Output   |
/// |----------------------------------|----------|
/// | `getWorkspacesIdExpertunities`   | `GET`    |
/// | `postAdminAgents`                | `POST`   |
/// | `deleteWorkspacesId`             | `DELETE` |
/// | `patchResourcesId`               | `PATCH`  |
/// | `putWorkspacesIdDocument`        | `PUT`    |
/// | `unknownPrefix`                  | `GET`    |  (default)
///
/// Returns the method in uppercase. Defaults to `"GET"` for unrecognised prefixes.
fn infer_method_from_sdk_fn_name(name: &str) -> String {
    let name_lower = name.to_lowercase();
    for &(prefix, method) in &[
        ("delete", "DELETE"),
        ("patch", "PATCH"),
        ("post", "POST"),
        ("put", "PUT"),
        ("head", "HEAD"),
        ("options", "OPTIONS"),
        ("get", "GET"),
    ] {
        if name_lower.starts_with(prefix) {
            // Verify the character after the prefix is uppercase (camelCase boundary)
            // or the name IS the prefix (e.g. a function literally named "get").
            let rest = &name[prefix.len()..];
            if rest.is_empty() || rest.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                return method.to_string();
            }
        }
    }
    "GET".to_string()
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
/// - `api_client.py` / `api_client.go` (starts with `api_client.`)
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
        || file_name.starts_with("api_client.")
}

/// Public re-export of [`is_generated_sdk_file`] for use by
/// `sdk_path_inference_pass` in the sibling module.
pub fn is_generated_sdk_file_pub(path: &std::path::Path) -> bool {
    is_generated_sdk_file(path)
}

// ---------------------------------------------------------------------------
// Pass
// ---------------------------------------------------------------------------

/// Post-extraction pass: emit `Implements` edges from generated SDK functions
/// to their matching `ApiEndpoint` nodes.
///
/// Uses two matching strategies (in order):
///
/// 1. **operation_id** — matches SDK function names against `operation_id`
///    metadata on `ApiEndpoint` nodes (normalised case/underscore-insensitive).
///    Works for repos with a checked-in OpenAPI spec.
///
/// 2. **URL path co-location** — for SDK generators (like `@hey-api/openapi-ts`)
///    that embed the full server URL as a string literal inside each function,
///    matches via `Const` nodes on the same line as the SDK Function.
///    Works when no spec file is present but the SDK encodes the URL inline.
///
/// Call this **after all nodes have been merged** across roots. The SDK file and
/// the OpenAPI spec are typically in different roots (or at least different files);
/// running this pass per-delta would miss cross-file links.
pub fn openapi_sdk_link_pass(all_nodes: &[Node]) -> Vec<Edge> {
    // Phase 1a: build a map from normalised operation_id → Vec<NodeId> for all
    // ApiEndpoint nodes that carry an `operation_id` in their metadata.
    let mut endpoint_by_op_id: HashMap<String, Vec<NodeId>> = HashMap::new();
    // Phase 1b: build a map from (normalised_path, normalised_method) → Vec<NodeId>
    // for all ApiEndpoint nodes (used by the URL co-location fallback strategy).
    // Using (path, method) instead of path alone avoids treating GET/POST on the
    // same path as ambiguous — each method is a distinct endpoint.
    let mut endpoint_by_path_method: HashMap<(String, String), Vec<NodeId>> = HashMap::new();

    for node in all_nodes {
        if node.id.kind != NodeKind::ApiEndpoint {
            continue;
        }
        if let Some(op_id) = node.metadata.get("operation_id")
            && !op_id.is_empty() {
                let key = normalize_operation_id(op_id);
                endpoint_by_op_id.entry(key).or_default().push(node.id.clone());
            }
        if let Some(http_path) = node.metadata.get("http_path")
            && !http_path.is_empty() {
                let norm_path = normalize_url_path(http_path);
                // Extract the HTTP method from the endpoint name (e.g. "GET /users/{id}")
                // or from the http_method metadata field.
                let method = node.metadata.get("http_method")
                    .map(|m| m.to_uppercase())
                    .unwrap_or_else(|| {
                        // Fall back to extracting from node name: "GET /path" → "GET"
                        node.id.name.split_whitespace().next()
                            .unwrap_or("GET")
                            .to_uppercase()
                    });
                endpoint_by_path_method.entry((norm_path, method)).or_default().push(node.id.clone());
            }
    }

    // Fast path: no ApiEndpoints means no edges to emit regardless of strategy.
    if endpoint_by_op_id.is_empty() && endpoint_by_path_method.is_empty() {
        return Vec::new();
    }

    // Phase 1c: build a map from (root, file, line) → Vec<normalized_path> for all
    // synthetic Const nodes (URL string literals) in generated SDK files.
    // These are the embedded URLs in SDK function bodies, e.g.:
    //   `{ url: '/workspaces/{id}/expertunities', ...options }`
    // The string_literals extractor creates a Const node for each URL string,
    // and it appears on the same line as the enclosing SDK function.
    //
    // Root is included in the key to prevent cross-root contamination in monorepos
    // where two roots may contain different versions of the same SDK filename.
    let mut url_consts_by_root_file_line: HashMap<(String, PathBuf, usize), Vec<String>> = HashMap::new();
    if !endpoint_by_path_method.is_empty() {
        for node in all_nodes {
            if node.id.kind != NodeKind::Const {
                continue;
            }
            // Only synthetic Const nodes (string literals from harvest_string_literals).
            if !node.metadata.get("synthetic").map(|s| s == "true").unwrap_or(false) {
                continue;
            }
            if !is_generated_sdk_file(&node.id.file) {
                continue;
            }
            // Only URL-shaped values (starts with '/', length > 1).
            let path_str = &node.id.name;
            if !path_str.starts_with('/') || path_str.len() <= 1 {
                continue;
            }
            let key = (node.id.root.clone(), node.id.file.clone(), node.line_start);
            url_consts_by_root_file_line
                .entry(key)
                .or_default()
                .push(normalize_url_path(path_str));
        }
    }

    // Phase 2: scan Function nodes in generated SDK files and match against the maps.
    let mut edges = Vec::new();
    for node in all_nodes {
        if node.id.kind != NodeKind::Function {
            continue;
        }
        if !is_generated_sdk_file(&node.id.file) {
            continue;
        }

        // Strategy 1: operation_id match (primary).
        let op_id_key = normalize_operation_id(&node.id.name);
        if let Some(ep_ids) = endpoint_by_op_id.get(&op_id_key) {
            // Skip ambiguous matches: if multiple ApiEndpoint nodes share the same
            // normalized operation_id (e.g. two services in a monorepo both define
            // `listUsers`), linking to all of them would create false cross-service
            // edges. Only emit an edge when the match is unambiguous (exactly one
            // ApiEndpoint has this operation_id).
            if ep_ids.len() == 1 {
                edges.push(Edge {
                    from: node.id.clone(),
                    to: ep_ids[0].clone(),
                    kind: EdgeKind::Implements,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }
            continue; // strategy 1 matched (or was ambiguous) — skip strategy 2
        }

        // Strategy 2: URL path co-location fallback.
        // Look for URL-shaped Const nodes in the same root+file on the same line.
        // Each such Const is a string literal embedded in the SDK function body.
        if url_consts_by_root_file_line.is_empty() {
            continue;
        }

        // Infer the HTTP method from the SDK function name prefix (camelCase convention
        // used by @hey-api/openapi-ts: getXxx, postXxx, deleteXxx, etc.).
        // This lets us disambiguate GET /users from POST /users on the same line.
        let inferred_method = infer_method_from_sdk_fn_name(&node.id.name);

        let file_line_key = (node.id.root.clone(), node.id.file.clone(), node.line_start);
        if let Some(url_paths) = url_consts_by_root_file_line.get(&file_line_key) {
            // Collect all ApiEndpoint NodeIds reachable via any URL path on this line,
            // filtered to the inferred HTTP method.
            let mut matched_ep_ids: Vec<NodeId> = Vec::new();
            for norm_path in url_paths {
                let pm_key = (norm_path.clone(), inferred_method.clone());
                if let Some(ep_ids) = endpoint_by_path_method.get(&pm_key) {
                    matched_ep_ids.extend(ep_ids.iter().cloned());
                }
            }
            // Deduplicate (multiple URL strings on the same line could point to the
            // same endpoint after normalisation).
            matched_ep_ids.sort_by(|a, b| a.to_stable_id().cmp(&b.to_stable_id()));
            matched_ep_ids.dedup_by(|a, b| a.to_stable_id() == b.to_stable_id());

            // Only emit when there is exactly one unambiguous match to avoid
            // false cross-service edges in monorepos.
            if matched_ep_ids.len() == 1 {
                edges.push(Edge {
                    from: node.id.clone(),
                    to: matched_ep_ids.into_iter().next().unwrap(),
                    kind: EdgeKind::Implements,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            } else if matched_ep_ids.len() > 1 {
                tracing::debug!(
                    "openapi_sdk_link: skipping '{}' ({}:{}) — {} endpoints match URL path (ambiguous)",
                    node.id.name,
                    node.id.file.display(),
                    node.line_start,
                    matched_ep_ids.len(),
                );
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
        make_sdk_fn_at_line(name, file, 1)
    }

    fn make_sdk_fn_at_line(name: &str, file: &str, line: usize) -> Node {
        Node {
            id: NodeId {
                root: "frontend".to_string(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind: NodeKind::Function,
            },
            language: "typescript".to_string(),
            line_start: line,
            line_end: line,
            signature: format!("export async function {}(", name),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_api_endpoint(name: &str, op_id: &str, file: &str) -> Node {
        make_api_endpoint_with_path(name, op_id, "", file)
    }

    fn make_api_endpoint_with_path(name: &str, op_id: &str, http_path: &str, file: &str) -> Node {
        let mut metadata = BTreeMap::new();
        if !op_id.is_empty() {
            metadata.insert("operation_id".to_string(), op_id.to_string());
        }
        if !http_path.is_empty() {
            metadata.insert("http_path".to_string(), http_path.to_string());
        }
        // Extract http_method from node name (e.g. "GET /workspaces/{id}/foo" → "GET").
        // If the name starts with a known HTTP method, set it in metadata so the pass
        // can use (path, method) keying for unambiguous matching.
        let method = name.split_whitespace().next().unwrap_or("GET").to_uppercase();
        if ["GET","POST","PUT","DELETE","PATCH","HEAD","OPTIONS"].contains(&method.as_str()) {
            metadata.insert("http_method".to_string(), method);
        }
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

    /// Make a synthetic URL Const node (as string_literals.rs would produce).
    fn make_url_const(url_path: &str, file: &str, line: usize) -> Node {
        let mut metadata = BTreeMap::new();
        metadata.insert("synthetic".to_string(), "true".to_string());
        Node {
            id: NodeId {
                root: "frontend".to_string(),
                file: PathBuf::from(file),
                name: url_path.to_string(),
                kind: NodeKind::Const,
            },
            language: "typescript".to_string(),
            line_start: line,
            line_end: line,
            signature: format!("\"{}\"", url_path),
            body: String::new(),
            metadata,
            source: ExtractionSource::TreeSitter,
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

    // --- normalize_url_path ---

    #[test]
    fn test_normalize_url_path_curly_param() {
        assert_eq!(normalize_url_path("/users/{id}"), "/users/<param>");
    }

    #[test]
    fn test_normalize_url_path_colon_param() {
        assert_eq!(normalize_url_path("/users/:id"), "/users/<param>");
    }

    #[test]
    fn test_normalize_url_path_bracket_param() {
        assert_eq!(normalize_url_path("/users/[id]"), "/users/<param>");
    }

    #[test]
    fn test_normalize_url_path_no_params() {
        assert_eq!(normalize_url_path("/workspaces/expertunities"), "/workspaces/expertunities");
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
    fn test_api_client_py_detected() {
        assert!(is_generated_sdk_file(&PathBuf::from("src/api_client.py")));
    }

    #[test]
    fn test_api_client_go_detected() {
        assert!(is_generated_sdk_file(&PathBuf::from("pkg/api_client.go")));
    }

    #[test]
    fn test_regular_service_not_detected() {
        assert!(!is_generated_sdk_file(&PathBuf::from("src/services/user_service.ts")));
    }

    #[test]
    fn test_plain_ts_not_detected() {
        assert!(!is_generated_sdk_file(&PathBuf::from("src/api/users.ts")));
    }

    #[test]
    fn test_plain_client_py_not_detected() {
        // "client.py" alone is too generic — don't match it without a "generated" qualifier
        assert!(!is_generated_sdk_file(&PathBuf::from("src/client.py")));
    }

    // --- openapi_sdk_link_pass: strategy 1 (operation_id) ---

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
    fn test_endpoint_without_operation_id_not_linked_by_strategy1() {
        // ApiEndpoint with no operation_id metadata — strategy 1 must not match.
        // (strategy 2 would need a Const node too; no Const here → still no edge)
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

        assert!(edges.is_empty(), "endpoint without operation_id must not match via strategy 1");
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

    /// Adversarial: ambiguous operation_id (multiple ApiEndpoints share the same
    /// normalized op_id, e.g. two services in a monorepo both define `listUsers`).
    /// Must NOT emit edges to avoid false cross-service links.
    #[test]
    fn test_ambiguous_op_id_skipped() {
        let nodes = vec![
            make_sdk_fn("listUsers", "src/api/sdk.gen.ts"),
            // Two services both have listUsers
            make_api_endpoint("GET /users", "listUsers", "service-a/openapi/api.yaml"),
            make_api_endpoint("GET /users", "listUsers", "service-b/openapi/api.yaml"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert!(
            edges.is_empty(),
            "ambiguous op_id (2 endpoints) must produce no edge, got: {:?}", edges
        );
    }

    /// Verify single-match still works after ambiguity guard.
    #[test]
    fn test_unique_op_id_still_links() {
        let nodes = vec![
            make_sdk_fn("listUsers", "src/api/sdk.gen.ts"),
            make_api_endpoint("GET /users", "listUsers", "openapi/api.yaml"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert_eq!(edges.len(), 1, "unique op_id must still produce edge");
    }

    // --- openapi_sdk_link_pass: strategy 2 (URL co-location) ---

    /// Happy path: hey-api/openapi-ts style SDK — function and URL Const on the same line,
    /// ApiEndpoint has http_path but no operation_id.
    #[test]
    fn test_url_colocation_emits_implements_edge() {
        let sdk_file = "client/src/api.gen/sdk.gen.ts";
        let nodes = vec![
            // SDK function at line 10
            make_sdk_fn_at_line("getWorkspacesIdExpertunities", sdk_file, 10),
            // URL string literal on the same line (as hey-api/openapi-ts generates)
            make_url_const("/workspaces/{id}/expertunities", sdk_file, 10),
            // FastAPI ApiEndpoint with no operation_id but with http_path
            make_api_endpoint_with_path(
                "GET /workspaces/{id}/expertunities",
                "",
                "/workspaces/{id}/expertunities",
                "ai_service/src/api/workspaces/id/expertunities.py",
            ),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert_eq!(edges.len(), 1, "URL co-location should produce 1 Implements edge, got: {:?}", edges);
        let e = &edges[0];
        assert_eq!(e.kind, EdgeKind::Implements);
        assert_eq!(e.from.name, "getWorkspacesIdExpertunities");
        assert_eq!(e.to.name, "GET /workspaces/{id}/expertunities");
    }

    /// URL co-location: path parameter styles should compare equal after normalisation.
    #[test]
    fn test_url_colocation_normalises_param_styles() {
        let sdk_file = "client/src/api.gen/sdk.gen.ts";
        // SDK has {id} style, endpoint also has {id} — should match after normalisation.
        let nodes = vec![
            make_sdk_fn_at_line("getWorkspacesId", sdk_file, 5),
            make_url_const("/workspaces/{id}", sdk_file, 5),
            make_api_endpoint_with_path(
                "GET /workspaces/{id}",
                "",
                "/workspaces/{id}",
                "src/api/workspaces.py",
            ),
        ];
        let edges = openapi_sdk_link_pass(&nodes);
        assert_eq!(edges.len(), 1, "param-normalised path match must produce edge");
    }

    /// URL co-location: Const node on a DIFFERENT line must not link.
    #[test]
    fn test_url_colocation_different_line_no_edge() {
        let sdk_file = "client/src/api.gen/sdk.gen.ts";
        let nodes = vec![
            make_sdk_fn_at_line("getWorkspacesIdExpertunities", sdk_file, 10),
            // Const at line 20 — NOT co-located
            make_url_const("/workspaces/{id}/expertunities", sdk_file, 20),
            make_api_endpoint_with_path(
                "GET /workspaces/{id}/expertunities",
                "",
                "/workspaces/{id}/expertunities",
                "ai_service/src/api/workspaces/id/expertunities.py",
            ),
        ];
        let edges = openapi_sdk_link_pass(&nodes);
        assert!(
            edges.is_empty(),
            "Const on different line must not produce edge, got: {:?}", edges
        );
    }

    /// URL co-location: Const in a DIFFERENT file must not link.
    #[test]
    fn test_url_colocation_different_file_no_edge() {
        let sdk_file = "client/src/api.gen/sdk.gen.ts";
        let nodes = vec![
            make_sdk_fn_at_line("getWorkspacesIdExpertunities", sdk_file, 10),
            // Const in a different file
            make_url_const("/workspaces/{id}/expertunities", "client/src/other.ts", 10),
            make_api_endpoint_with_path(
                "GET /workspaces/{id}/expertunities",
                "",
                "/workspaces/{id}/expertunities",
                "ai_service/src/api/workspaces/id/expertunities.py",
            ),
        ];
        let edges = openapi_sdk_link_pass(&nodes);
        assert!(
            edges.is_empty(),
            "Const in different file must not produce edge, got: {:?}", edges
        );
    }

    /// URL co-location: non-SDK file must not produce edge.
    #[test]
    fn test_url_colocation_non_sdk_file_no_edge() {
        let non_sdk_file = "client/src/services/user_service.ts";
        let nodes = vec![
            make_sdk_fn_at_line("getWorkspacesIdExpertunities", non_sdk_file, 10),
            make_url_const("/workspaces/{id}/expertunities", non_sdk_file, 10),
            make_api_endpoint_with_path(
                "GET /workspaces/{id}/expertunities",
                "",
                "/workspaces/{id}/expertunities",
                "ai_service/src/api/workspaces/id/expertunities.py",
            ),
        ];
        let edges = openapi_sdk_link_pass(&nodes);
        assert!(
            edges.is_empty(),
            "function in non-SDK file must not produce edge via URL co-location, got: {:?}", edges
        );
    }

    /// URL co-location: non-synthetic Const must not be used for matching.
    #[test]
    fn test_url_colocation_non_synthetic_const_no_edge() {
        let sdk_file = "client/src/api.gen/sdk.gen.ts";
        let mut non_synthetic_const = make_url_const("/workspaces/{id}/expertunities", sdk_file, 10);
        // Remove the synthetic flag (or set to false)
        non_synthetic_const.metadata.insert("synthetic".to_string(), "false".to_string());

        let nodes = vec![
            make_sdk_fn_at_line("getWorkspacesIdExpertunities", sdk_file, 10),
            non_synthetic_const,
            make_api_endpoint_with_path(
                "GET /workspaces/{id}/expertunities",
                "",
                "/workspaces/{id}/expertunities",
                "ai_service/src/api/workspaces/id/expertunities.py",
            ),
        ];
        let edges = openapi_sdk_link_pass(&nodes);
        assert!(
            edges.is_empty(),
            "non-synthetic Const must not be used for URL co-location, got: {:?}", edges
        );
    }

    /// URL co-location: when ApiEndpoint has operation_id AND matching URL Const,
    /// strategy 1 takes precedence and no duplicate edge is emitted.
    #[test]
    fn test_strategy1_takes_precedence_over_strategy2() {
        let sdk_file = "client/src/api.gen/sdk.gen.ts";
        let nodes = vec![
            make_sdk_fn_at_line("listUsers", sdk_file, 10),
            make_url_const("/users", sdk_file, 10),
            // endpoint has BOTH operation_id AND http_path
            make_api_endpoint_with_path("GET /users", "listUsers", "/users", "openapi/api.yaml"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);
        // Exactly one edge — strategy 1 matched first, strategy 2 was skipped.
        assert_eq!(edges.len(), 1, "exactly one edge, no duplicate from strategy 2");
        assert_eq!(edges[0].from.name, "listUsers");
    }

    /// URL co-location: ambiguous path (two endpoints share the same http_path after
    /// normalisation) — must NOT emit any edge.
    #[test]
    fn test_url_colocation_ambiguous_path_no_edge() {
        let sdk_file = "client/src/api.gen/sdk.gen.ts";
        let nodes = vec![
            make_sdk_fn_at_line("getWorkspacesId", sdk_file, 10),
            make_url_const("/workspaces/{id}", sdk_file, 10),
            // Two endpoints share the same normalised path
            make_api_endpoint_with_path(
                "GET /workspaces/{id}",
                "",
                "/workspaces/{id}",
                "service-a/src/api/workspaces.py",
            ),
            make_api_endpoint_with_path(
                "GET /workspaces/{id}",
                "",
                "/workspaces/{id}",
                "service-b/src/api/workspaces.py",
            ),
        ];
        let edges = openapi_sdk_link_pass(&nodes);
        assert!(
            edges.is_empty(),
            "ambiguous path must not produce edge, got: {:?}", edges
        );
    }

    /// URL co-location: multiple SDK functions in same file, each on a different
    /// line with different URLs, must each link to their own endpoint.
    #[test]
    fn test_url_colocation_multiple_functions_independent() {
        let sdk_file = "client/src/api.gen/sdk.gen.ts";
        let nodes = vec![
            make_sdk_fn_at_line("getWorkspacesIdExpertunities", sdk_file, 10),
            make_url_const("/workspaces/{id}/expertunities", sdk_file, 10),
            make_sdk_fn_at_line("getWorkspacesIdActivities", sdk_file, 20),
            make_url_const("/workspaces/{id}/activities", sdk_file, 20),
            make_api_endpoint_with_path(
                "GET /workspaces/{id}/expertunities",
                "",
                "/workspaces/{id}/expertunities",
                "ai_service/src/api/workspaces/id/expertunities.py",
            ),
            make_api_endpoint_with_path(
                "GET /workspaces/{id}/activities",
                "",
                "/workspaces/{id}/activities",
                "ai_service/src/api/workspaces/id/activities.py",
            ),
        ];
        let edges = openapi_sdk_link_pass(&nodes);
        assert_eq!(edges.len(), 2, "two functions must each link to their endpoint");
        let from_names: Vec<_> = edges.iter().map(|e| e.from.name.as_str()).collect();
        assert!(from_names.contains(&"getWorkspacesIdExpertunities"));
        assert!(from_names.contains(&"getWorkspacesIdActivities"));
    }

    /// CodeRabbit Finding #1: cross-root contamination prevention.
    ///
    /// When two roots both have an SDK file with the same relative path, a function
    /// in root A must NOT match a URL Const from root B even on the same file+line.
    #[test]
    fn test_url_colocation_cross_root_contamination_prevented() {
        let sdk_file = "src/api/sdk.gen.ts"; // same path in both roots

        // Root A: SDK function at line 5
        let mut sdk_fn_a = make_sdk_fn_at_line("getItems", sdk_file, 5);
        sdk_fn_a.id.root = "service-a".to_string();

        // Root B: URL const at line 5 (same file path, same line, BUT different root)
        let mut url_const_b = make_url_const("/items", sdk_file, 5);
        url_const_b.id.root = "service-b".to_string();

        // Endpoint in root B
        let mut ep_b = make_api_endpoint_with_path(
            "GET /items", "", "/items", "service-b/src/api/routes.py",
        );
        ep_b.id.root = "service-b".to_string();

        let nodes = vec![sdk_fn_a, url_const_b, ep_b];
        let edges = openapi_sdk_link_pass(&nodes);

        assert!(
            edges.is_empty(),
            "cross-root URL Const must not pollute SDK function from a different root, got: {:?}", edges
        );
    }

    /// CodeRabbit Finding #1 (positive case): same root works correctly.
    #[test]
    fn test_url_colocation_same_root_links() {
        let sdk_file = "src/api/sdk.gen.ts";

        let mut sdk_fn = make_sdk_fn_at_line("getItems", sdk_file, 5);
        sdk_fn.id.root = "service-a".to_string();

        let mut url_const = make_url_const("/items", sdk_file, 5);
        url_const.id.root = "service-a".to_string(); // same root as function

        let mut ep = make_api_endpoint_with_path(
            "GET /items", "", "/items", "service-a/src/api/routes.py",
        );
        ep.id.root = "service-a".to_string();

        let nodes = vec![sdk_fn, url_const, ep];
        let edges = openapi_sdk_link_pass(&nodes);

        assert_eq!(edges.len(), 1, "same-root URL Const must produce edge");
    }

    /// CodeRabbit Finding #2: GET and POST on the same path are disambiguated by method.
    ///
    /// When both GET /users and POST /users exist, `getUsers` SDK function should
    /// link only to GET /users (not be treated as ambiguous because POST /users also exists).
    #[test]
    fn test_url_colocation_get_and_post_same_path_disambiguated() {
        let sdk_file = "client/src/api.gen/sdk.gen.ts";
        let nodes = vec![
            // SDK functions infer method from name prefix
            make_sdk_fn_at_line("getUsers", sdk_file, 10),
            make_url_const("/users", sdk_file, 10),
            make_sdk_fn_at_line("postUsers", sdk_file, 20),
            make_url_const("/users", sdk_file, 20),
            // Two endpoints on the same path, different methods
            make_api_endpoint_with_path("GET /users", "", "/users", "src/api/users.py"),
            make_api_endpoint_with_path("POST /users", "", "/users", "src/api/users.py"),
        ];
        let edges = openapi_sdk_link_pass(&nodes);

        assert_eq!(edges.len(), 2, "GET and POST functions must each link independently, got: {:?}", edges);
        let get_edge = edges.iter().find(|e| e.from.name == "getUsers");
        let post_edge = edges.iter().find(|e| e.from.name == "postUsers");
        assert!(get_edge.is_some(), "getUsers must have an edge");
        assert!(post_edge.is_some(), "postUsers must have an edge");
        assert_eq!(get_edge.unwrap().to.name, "GET /users", "getUsers must link to GET endpoint");
        assert_eq!(post_edge.unwrap().to.name, "POST /users", "postUsers must link to POST endpoint");
    }

    /// Verify infer_method_from_sdk_fn_name helper.
    #[test]
    fn test_infer_method_from_sdk_fn_name() {
        assert_eq!(infer_method_from_sdk_fn_name("getWorkspacesId"), "GET");
        assert_eq!(infer_method_from_sdk_fn_name("postAdminAgents"), "POST");
        assert_eq!(infer_method_from_sdk_fn_name("deleteWorkspacesId"), "DELETE");
        assert_eq!(infer_method_from_sdk_fn_name("patchResourcesId"), "PATCH");
        assert_eq!(infer_method_from_sdk_fn_name("putWorkspacesIdDocument"), "PUT");
        assert_eq!(infer_method_from_sdk_fn_name("headHealth"), "HEAD");
        assert_eq!(infer_method_from_sdk_fn_name("optionsHealth"), "OPTIONS");
        // Default for unknown prefixes
        assert_eq!(infer_method_from_sdk_fn_name("unknownFunction"), "GET");
        // Single-word names that ARE the prefix (e.g. a function literally named "get")
        assert_eq!(infer_method_from_sdk_fn_name("get"), "GET");
        assert_eq!(infer_method_from_sdk_fn_name("post"), "POST");
    }
}
