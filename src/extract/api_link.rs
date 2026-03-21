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

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, NodeKind};

use super::ExtractionResult;

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
/// Call this after all per-file extraction is complete so both node types are
/// available. The pass is O(C × A) where C is the number of URL-shaped `Const`
/// nodes and A is the number of `ApiEndpoint` nodes — both are typically small.
///
/// Edges are appended to `result.edges` in-place.
pub fn api_link_pass(result: &mut ExtractionResult) {
    // Collect URL-shaped Const nodes and ApiEndpoint nodes separately so we
    // don't borrow `result.nodes` mutably while iterating.
    // Only match synthetic Const nodes (string literals from harvest_string_literals).
    // Non-synthetic Const nodes are actual declared constants whose names may
    // coincidentally start with '/' but are not URL path strings.
    let url_consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| {
            n.id.kind == NodeKind::Const
                && n.metadata.get("synthetic").map(|s| s == "true").unwrap_or(false)
                && looks_like_url_path(&n.id.name)
        })
        .map(|n| (n.id.clone(), normalize_path(&n.id.name)))
        .collect();

    if url_consts.is_empty() {
        return;
    }

    let endpoints: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
        .filter_map(|n| {
            let http_path = n.metadata.get("http_path")?;
            Some((n.id.clone(), normalize_path(http_path)))
        })
        .collect();

    if endpoints.is_empty() {
        return;
    }

    for (const_id, const_norm) in &url_consts {
        for (ep_id, ep_norm) in &endpoints {
            if const_norm == ep_norm {
                result.edges.push(Edge {
                    from: const_id.clone(),
                    to: ep_id.clone(),
                    kind: EdgeKind::DependsOn,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, NodeKind};

    fn make_const(name: &str, file: &str) -> crate::graph::Node {
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        use crate::graph::{ExtractionSource, NodeId};
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
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        use crate::graph::{ExtractionSource, NodeId};
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
        let mut result = ExtractionResult::default();
        result.nodes.push(make_const("/payments", "client.py"));
        result.nodes.push(make_endpoint("POST", "/payments", "routes.py"));

        api_link_pass(&mut result);

        assert_eq!(result.edges.len(), 1, "should emit exactly 1 DependsOn edge");
        let edge = &result.edges[0];
        assert_eq!(edge.kind, EdgeKind::DependsOn);
        assert_eq!(edge.from.kind, NodeKind::Const);
        assert_eq!(edge.from.name, "/payments");
        assert_eq!(edge.to.kind, NodeKind::ApiEndpoint);
        assert_eq!(edge.to.name, "POST /payments");
    }

    /// Express-style `:id` matches Spring-style `{id}` via normalisation.
    #[test]
    fn test_param_normalization_colon_matches_braces() {
        let mut result = ExtractionResult::default();
        result.nodes.push(make_const("/users/:id", "client.js"));
        result.nodes.push(make_endpoint("GET", "/users/{id}", "routes.py"));

        api_link_pass(&mut result);

        assert_eq!(result.edges.len(), 1, "should match :id to {{id}} via normalisation");
    }

    /// Next.js `[id]` bracket style also normalises correctly.
    #[test]
    fn test_param_normalization_brackets() {
        let mut result = ExtractionResult::default();
        result.nodes.push(make_const("/users/[id]", "pages.tsx"));
        result.nodes.push(make_endpoint("GET", "/users/{id}", "routes.py"));

        api_link_pass(&mut result);

        assert_eq!(result.edges.len(), 1, "should match [id] to {{id}} via normalisation");
    }

    /// Non-URL strings (e.g. MIME types) must NOT produce edges.
    #[test]
    fn test_non_path_string_does_not_link() {
        let mut result = ExtractionResult::default();
        result.nodes.push(make_const("application/json", "client.py"));
        result.nodes.push(make_endpoint("POST", "/payments", "routes.py"));

        api_link_pass(&mut result);

        assert!(
            result.edges.is_empty(),
            "non-path string should produce no edge, got: {:?}", result.edges
        );
    }

    /// No endpoints → no edges even if there are URL-shaped strings.
    #[test]
    fn test_no_endpoints_no_edges() {
        let mut result = ExtractionResult::default();
        result.nodes.push(make_const("/payments", "client.py"));

        api_link_pass(&mut result);

        assert!(result.edges.is_empty());
    }

    /// No URL-shaped Const nodes → no edges even if there are endpoints.
    #[test]
    fn test_no_url_consts_no_edges() {
        let mut result = ExtractionResult::default();
        result.nodes.push(make_endpoint("GET", "/users", "routes.py"));

        api_link_pass(&mut result);

        assert!(result.edges.is_empty());
    }

    /// Multiple Const nodes can link to the same endpoint.
    #[test]
    fn test_multiple_consts_link_to_same_endpoint() {
        let mut result = ExtractionResult::default();
        result.nodes.push(make_const("/payments", "client_a.py"));
        result.nodes.push(make_const("/payments", "client_b.py"));
        result.nodes.push(make_endpoint("POST", "/payments", "routes.py"));

        api_link_pass(&mut result);

        assert_eq!(result.edges.len(), 2, "both Const nodes should link to the endpoint");
    }

    /// `normalize_path` handles multiple parameters in one path.
    #[test]
    fn test_normalize_path_multiple_params() {
        assert_eq!(
            normalize_path("/a/:x/b/:y"),
            "/a/<param>/b/<param>"
        );
        assert_eq!(
            normalize_path("/a/{x}/b/{y}"),
            "/a/<param>/b/<param>"
        );
    }

    /// The lone `/` root path is NOT treated as a URL path to avoid spurious matches.
    #[test]
    fn test_root_slash_not_treated_as_url() {
        assert!(!looks_like_url_path("/"), "bare / should not be treated as a URL path");
    }

    /// Static segments are preserved as-is.
    #[test]
    fn test_normalize_path_static_segments_unchanged() {
        assert_eq!(normalize_path("/api/v1/payments"), "/api/v1/payments");
    }
}
