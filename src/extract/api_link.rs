//! Post-extraction pass that links string-literal `Const` nodes whose names look
//! like URL paths to matching `ApiEndpoint` nodes via `DependsOn` edges.
//!
//! # Problem
//!
//! After tree-sitter extraction two node types exist in the graph that
//! *should* be related but have no edge between them:
//!
//! - **`NodeKind::Const`** (synthetic) — string literals such as `"/payments"`,
//!   `"/users/{id}"`, or `"/api/v1/items"` emitted by `string_literals.rs`.
//! - **`NodeKind::ApiEndpoint`** — route nodes such as `POST /payments` emitted
//!   by `run_route_queries` in `generic.rs`.
//!
//! An agent asking "which code calls the payments endpoint?" cannot answer the
//! question without this edge.
//!
//! # Solution
//!
//! [`api_link_pass`] scans all nodes in an [`ExtractionResult`], normalises URL
//! path parameters (`:id` ≡ `{id}`), and emits a `DependsOn` edge from each
//! matching `Const` → `ApiEndpoint`.
//!
//! # Parameter normalisation
//!
//! Path parameters appear in different forms across frameworks:
//!
//! | Style | Example |
//! |-------|---------|
//! | Express / Rails | `/users/:id` |
//! | Spring / FastAPI | `/users/{id}` |
//! | Next.js | `/users/[id]` |
//!
//! All three are normalised to a common `/users/<param>` form before
//! comparison so that a `"/users/:id"` string literal matches a
//! `GET /users/{id}` endpoint.

use std::collections::HashMap;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

/// Returns `true` if `s` looks like a URL path: starts with `/` and has length
/// greater than 1 (excludes the root `/` alone, which matches everything).
///
/// This is intentionally permissive — false positives (non-route strings that
/// start with `/`) are cheap; false negatives (missed links) are costly.
fn looks_like_url_path(s: &str) -> bool {
    s.starts_with('/') && s.len() > 1
}

/// Normalise a URL path so that path-parameter variants compare equal.
///
/// | Input | Output |
/// |-------|--------|
/// | `/users/:id` | `/users/<param>` |
/// | `/users/{id}` | `/users/<param>` |
/// | `/users/[id]` | `/users/<param>` |
///
/// Static segments and multiple parameters are all handled:
/// `/a/:x/b/:y` → `/a/<param>/b/<param>`.
pub fn normalize_path(path: &str) -> String {
    // Split on `/`, normalise each segment, rejoin.
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

/// Post-extraction pass: link string-literal `Const` nodes that look like URL
/// paths to matching `ApiEndpoint` nodes via `DependsOn` edges.
///
/// Call this **after all nodes from all roots have been merged** (i.e., in
/// `build_full_graph` after `all_nodes` is fully populated), not per-file or
/// per-delta. The reason: the `Const` caller may be in a different file from
/// the `ApiEndpoint` it references, and incremental scans only process changed
/// files. Running this pass on the full node set ensures cross-file links are
/// always created correctly.
///
/// Complexity: O(C + E) using a HashMap keyed by normalised path, where C is
/// the count of URL-shaped `Const` nodes and E is the count of `ApiEndpoint`
/// nodes — both are typically small.
///
/// Returns the new edges to add.
pub fn api_link_pass(all_nodes: &[Node]) -> Vec<Edge> {
    // Build a HashMap from normalised endpoint path → endpoint NodeId.
    // Multiple endpoints can share the same normalised path (e.g., GET and
    // POST both at /users/{id} after normalisation), so we store a Vec.
    let mut endpoint_map: HashMap<String, Vec<NodeId>> = HashMap::new();
    for node in all_nodes {
        if node.id.kind != NodeKind::ApiEndpoint {
            continue;
        }
        if let Some(http_path) = node.metadata.get("http_path") {
            let norm = normalize_path(http_path);
            endpoint_map.entry(norm).or_default().push(node.id.clone());
        }
    }

    if endpoint_map.is_empty() {
        return Vec::new();
    }

    let mut edges = Vec::new();

    // Only match synthetic Const nodes (string literals from harvest_string_literals).
    // Non-synthetic Const nodes are actual declared constants whose names may
    // coincidentally start with '/' but are not URL path strings.
    for node in all_nodes {
        if node.id.kind != NodeKind::Const {
            continue;
        }
        if !node
            .metadata
            .get("synthetic")
            .map(|s| s == "true")
            .unwrap_or(false)
        {
            continue;
        }
        if !looks_like_url_path(&node.id.name) {
            continue;
        }

        let norm = normalize_path(&node.id.name);
        if let Some(ep_ids) = endpoint_map.get(&norm) {
            for ep_id in ep_ids {
                edges.push(Edge {
                    from: node.id.clone(),
                    to: ep_id.clone(),
                    kind: EdgeKind::DependsOn,
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
    use crate::graph::{EdgeKind, NodeKind};

    fn make_const(name: &str, file: &str) -> crate::graph::Node {
        use crate::graph::{ExtractionSource, NodeId};
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        crate::graph::Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind: NodeKind::Const,
            },
            language: "python".to_string(),
            line_start: 1,
            line_end: 1,
            signature: format!("\"{}\"", name),
            body: String::new(),
            metadata: {
                let mut m = BTreeMap::new();
                m.insert("synthetic".to_string(), "true".to_string());
                m.insert("value".to_string(), name.to_string());
                m
            },
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_endpoint(method: &str, path: &str, file: &str) -> crate::graph::Node {
        use crate::graph::{ExtractionSource, NodeId};
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        let name = format!("{} {}", method, path);
        crate::graph::Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from(file),
                name: name.clone(),
                kind: NodeKind::ApiEndpoint,
            },
            language: "python".to_string(),
            line_start: 1,
            line_end: 2,
            signature: format!("[route_decorator] {} {}", method, path),
            body: String::new(),
            metadata: {
                let mut m = BTreeMap::new();
                m.insert("http_method".to_string(), method.to_string());
                m.insert("http_path".to_string(), path.to_string());
                m
            },
            source: ExtractionSource::TreeSitter,
        }
    }

    /// Exact path match: `/payments` Const → `POST /payments` ApiEndpoint.
    #[test]
    fn test_exact_path_match_emits_depends_on_edge() {
        let nodes = vec![
            make_const("/payments", "client.py"),
            make_endpoint("POST", "/payments", "routes.py"),
        ];
        let edges = api_link_pass(&nodes);

        assert_eq!(edges.len(), 1, "should emit exactly 1 DependsOn edge");
        let edge = &edges[0];
        assert_eq!(edge.kind, EdgeKind::DependsOn);
        assert_eq!(edge.from.kind, NodeKind::Const);
        assert_eq!(edge.from.name, "/payments");
        assert_eq!(edge.to.kind, NodeKind::ApiEndpoint);
        assert_eq!(edge.to.name, "POST /payments");
    }

    /// Express-style `:id` matches Spring-style `{id}` via normalisation.
    #[test]
    fn test_param_normalization_colon_matches_braces() {
        let nodes = vec![
            make_const("/users/:id", "client.js"),
            make_endpoint("GET", "/users/{id}", "routes.py"),
        ];
        let edges = api_link_pass(&nodes);

        assert_eq!(
            edges.len(),
            1,
            "should match :id to {{id}} via normalisation"
        );
    }

    /// Next.js `[id]` bracket style also normalises correctly.
    #[test]
    fn test_param_normalization_brackets() {
        let nodes = vec![
            make_const("/users/[id]", "pages.tsx"),
            make_endpoint("GET", "/users/{id}", "routes.py"),
        ];
        let edges = api_link_pass(&nodes);

        assert_eq!(
            edges.len(),
            1,
            "should match [id] to {{id}} via normalisation"
        );
    }

    /// Non-URL strings (e.g. MIME types) must NOT produce edges.
    #[test]
    fn test_non_path_string_does_not_link() {
        let nodes = vec![
            make_const("application/json", "client.py"),
            make_endpoint("POST", "/payments", "routes.py"),
        ];
        let edges = api_link_pass(&nodes);

        assert!(
            edges.is_empty(),
            "non-path string should produce no edge, got: {:?}",
            edges
        );
    }

    /// No endpoints → no edges even if there are URL-shaped strings.
    #[test]
    fn test_no_endpoints_no_edges() {
        let nodes = vec![make_const("/payments", "client.py")];
        let edges = api_link_pass(&nodes);
        assert!(edges.is_empty());
    }

    /// No URL-shaped Const nodes → no edges even if there are endpoints.
    #[test]
    fn test_no_url_consts_no_edges() {
        let nodes = vec![make_endpoint("GET", "/users", "routes.py")];
        let edges = api_link_pass(&nodes);
        assert!(edges.is_empty());
    }

    /// Multiple Const nodes can link to the same endpoint.
    #[test]
    fn test_multiple_consts_link_to_same_endpoint() {
        let nodes = vec![
            make_const("/payments", "client_a.py"),
            make_const("/payments", "client_b.py"),
            make_endpoint("POST", "/payments", "routes.py"),
        ];
        let edges = api_link_pass(&nodes);

        assert_eq!(
            edges.len(),
            2,
            "both Const nodes should link to the endpoint"
        );
    }

    /// Cross-file: Const in one file links to ApiEndpoint in a different file.
    /// This is the case that fails when api_link_pass runs per-delta instead
    /// of on the full merged node set.
    #[test]
    fn test_cross_file_const_links_to_endpoint() {
        let nodes = vec![
            make_const("/orders", "src/client/orders_client.py"),
            make_endpoint("POST", "/orders", "src/api/orders_routes.py"),
        ];
        let edges = api_link_pass(&nodes);

        assert_eq!(edges.len(), 1, "cross-file const should link to endpoint");
        assert_eq!(edges[0].from.name, "/orders");
        assert_eq!(edges[0].to.name, "POST /orders");
    }

    /// `normalize_path` handles multiple parameters in one path.
    #[test]
    fn test_normalize_path_multiple_params() {
        assert_eq!(normalize_path("/a/:x/b/:y"), "/a/<param>/b/<param>");
        assert_eq!(normalize_path("/a/{x}/b/{y}"), "/a/<param>/b/<param>");
    }

    /// The lone `/` root path is NOT treated as a URL path to avoid spurious matches.
    #[test]
    fn test_root_slash_not_treated_as_url() {
        assert!(
            !looks_like_url_path("/"),
            "bare / should not be treated as a URL path"
        );
    }

    /// Static segments are preserved as-is.
    #[test]
    fn test_normalize_path_static_segments_unchanged() {
        assert_eq!(normalize_path("/api/v1/payments"), "/api/v1/payments");
    }

    // -----------------------------------------------------------------------
    // Adversarial tests
    // -----------------------------------------------------------------------

    /// A Const node without `synthetic: true` metadata must NOT be linked,
    /// even if its name looks like a URL path. This prevents linking declared
    /// constants (e.g. `const REDIRECT_PATH: &str = "/admin"`) to endpoints.
    #[test]
    fn test_non_synthetic_const_not_linked() {
        use crate::graph::{ExtractionSource, NodeId, NodeKind};
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        let non_synthetic = crate::graph::Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("consts.rs"),
                name: "/admin".to_string(),
                kind: NodeKind::Const,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "const ADMIN_PATH: &str = \"/admin\"".to_string(),
            body: String::new(),
            metadata: {
                let mut m = BTreeMap::new();
                m.insert("synthetic".to_string(), "false".to_string()); // declared constant
                m
            },
            source: ExtractionSource::TreeSitter,
        };
        let nodes = vec![non_synthetic, make_endpoint("GET", "/admin", "routes.py")];
        let edges = api_link_pass(&nodes);

        assert!(
            edges.is_empty(),
            "declared constant should NOT link to endpoint, got: {:?}",
            edges
        );
    }

    /// `looks_like_url_path` does match short paths like `"/ok"` — that is
    /// intentional. The upstream `harvest_string_literals` filter (len > 3)
    /// ensures such short values never reach `api_link_pass` as Const nodes.
    /// This test documents the boundary: `"/ok"` matches the predicate,
    /// but `"/ok"` (len=3) would have been filtered before reaching this pass.
    #[test]
    fn test_short_url_path_matches_predicate_but_filtered_upstream() {
        // "/ok" technically satisfies looks_like_url_path (starts with '/', len > 1)
        assert!(looks_like_url_path("/ok"), "/ok passes looks_like_url_path");
        // But harvest_string_literals filters len > 3, so "/ok" (len=3) would not appear.
        // If it somehow does appear (future code change), api_link_pass would try to
        // match it. That's acceptable — a 3-char URL is unlikely to match any endpoint.
        // This test is a documentation test, not a constraint test.
    }

    /// Mismatched paths (/payments vs /payments/:id) must NOT produce edges.
    #[test]
    fn test_path_mismatch_no_edge() {
        let nodes = vec![
            make_const("/payments", "client.py"),
            make_endpoint("GET", "/payments/{id}", "routes.py"),
        ];
        let edges = api_link_pass(&nodes);

        assert!(
            edges.is_empty(),
            "/payments should NOT match /payments/{{id}}: got {:?}",
            edges
        );
    }

    /// Empty `http_path` in ApiEndpoint metadata must not panic or match anything.
    #[test]
    fn test_api_endpoint_without_http_path_metadata_does_not_panic() {
        use crate::graph::{ExtractionSource, NodeId, NodeKind};
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        let no_path_ep = crate::graph::Node {
            id: NodeId {
                root: String::new(),
                file: PathBuf::from("routes.py"),
                name: "GET ".to_string(),
                kind: NodeKind::ApiEndpoint,
            },
            language: "python".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "[route_decorator] GET ".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(), // no http_path key
            source: ExtractionSource::TreeSitter,
        };
        let nodes = vec![make_const("/payments", "client.py"), no_path_ep];

        // Must not panic
        let edges = api_link_pass(&nodes);
        // No match expected since the endpoint has no http_path
        assert!(edges.is_empty());
    }
}
