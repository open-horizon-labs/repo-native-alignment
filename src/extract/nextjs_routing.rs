//! Post-extraction pass that turns Next.js file-path API routes into
//! `NodeKind::ApiEndpoint` nodes.
//!
//! # Motivation
//!
//! Next.js uses the *filesystem path* as the route declaration — there are no
//! route decorators or function-call patterns for tree-sitter to query.
//! The existing `run_route_queries` in `generic.rs` handles decorator/
//! function-call patterns and misses Next.js entirely.  This pass fills that
//! gap by matching path patterns and reading the file content only where
//! necessary (App Router — to find exported HTTP-method functions).
//!
//! # Conventions supported
//!
//! ## App Router (`app/api/**/route.{ts,tsx,js}`)
//!
//! ```text
//! app/api/payments/route.ts  →  /payments   (exported GET, POST, …)
//! app/api/users/[id]/route.ts → /users/{id}  (exported GET, PUT, DELETE, …)
//! ```
//!
//! One `ApiEndpoint` node is emitted per exported HTTP-method function found
//! in the file (`GET`, `POST`, `PUT`, `DELETE`, `PATCH`).  An `Implements`
//! edge links each endpoint node to the corresponding `Function` node already
//! present in `existing_nodes`.
//!
//! ## Pages Router (`pages/api/**/*.{ts,tsx,js}`)
//!
//! ```text
//! pages/api/payments.ts  →  ANY /api/payments
//! pages/api/users/[id].ts → ANY /api/users/{id}
//! ```
//!
//! One `ApiEndpoint` node (method = `ANY`) is emitted per file.  An
//! `Implements` edge links it to the default-export function node if one
//! exists in `existing_nodes`.
//!
//! # Path derivation
//!
//! | Pattern | Input segment | Output segment |
//! |---------|---------------|----------------|
//! | App Router strip | `app/api/payments/route.ts` | `/payments` |
//! | Pages Router strip | `pages/api/payments.ts` | `/api/payments` |
//! | Dynamic segments | `[id]` | `{id}` |
//! | Catch-all | `[...slug]` | `{slug}` |
//!
//! # Integration
//!
//! Call `nextjs_routing_pass` **after** tree-sitter extraction has produced
//! `existing_nodes` (so that Implements edges can be built).  This is
//! consistent with the `api_link_pass`, `manifest_pass`, and
//! `naming_convention::tested_by_pass` pattern.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

// ---------------------------------------------------------------------------
// Public result type and entry point
// ---------------------------------------------------------------------------

/// Result of the Next.js routing pass: nodes and edges to merge into the graph.
#[derive(Debug, Default)]
pub struct NextjsRoutingResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Scan `roots` for Next.js API route files and emit `ApiEndpoint` nodes +
/// `Implements` edges.
///
/// Each element of `roots` is `(root_slug, root_path)`.
/// `existing_nodes` is the already-extracted node set — used to find Function
/// nodes so that Implements edges can be created.
///
/// Safe to call with an empty `roots` or `existing_nodes` slice.
pub fn nextjs_routing_pass(
    roots: &[(String, PathBuf)],
    existing_nodes: &[Node],
) -> NextjsRoutingResult {
    let mut result = NextjsRoutingResult::default();

    for (root_slug, root_path) in roots {
        let partial = scan_root(root_slug, root_path, existing_nodes);
        result.nodes.extend(partial.nodes);
        result.edges.extend(partial.edges);
    }

    result
}

// ---------------------------------------------------------------------------
// Per-root scanning
// ---------------------------------------------------------------------------

fn scan_root(root_slug: &str, root_path: &Path, existing_nodes: &[Node]) -> NextjsRoutingResult {
    let mut result = NextjsRoutingResult::default();

    // Build a lookup: (relative_file_path, function_name) → NodeId
    // Used to find handler Function nodes for Implements edges.
    let fn_index: std::collections::HashMap<(PathBuf, String), NodeId> = existing_nodes
        .iter()
        .filter(|n| n.id.root == root_slug && n.id.kind == NodeKind::Function)
        .map(|n| ((n.id.file.clone(), n.id.name.clone()), n.id.clone()))
        .collect();

    // Walk the root for App Router and Pages Router files.
    // We use a manual walk to avoid pulling in the `walkdir` crate — the
    // root may be large (node_modules) so we skip known-noise directories.
    walk_for_nextjs(root_path, &mut |abs_path: &Path| {
        let rel_path = match abs_path.strip_prefix(root_path) {
            Ok(r) => r,
            Err(_) => return,
        };

        let rel_str = rel_path.to_string_lossy();

        // Normalise path separators to '/' for cross-platform matching.
        let rel_forward: String = rel_str.replace('\\', "/");

        if is_app_router_route(&rel_forward) {
            process_app_router_file(
                root_slug,
                root_path,
                rel_path,
                &rel_forward,
                abs_path,
                &fn_index,
                &mut result,
            );
        } else if is_pages_router_route(&rel_forward) {
            process_pages_router_file(
                root_slug,
                rel_path,
                &rel_forward,
                abs_path,
                &fn_index,
                &mut result,
            );
        }
    });

    result
}

// ---------------------------------------------------------------------------
// Route pattern detection
// ---------------------------------------------------------------------------

/// Returns `true` if the relative path looks like a Next.js App Router API
/// route: `app/api/**/route.{ts,tsx,js,jsx}`.
fn is_app_router_route(rel: &str) -> bool {
    // Must be inside app/api/ and the filename must be route.{ts,tsx,js,jsx}
    let Some(idx) = rel.rfind('/') else { return false; };
    let filename = &rel[idx + 1..];
    let dir = &rel[..idx];

    matches!(
        filename,
        "route.ts" | "route.tsx" | "route.js" | "route.jsx"
    ) && (dir.starts_with("app/api/") || dir == "app/api")
}

/// Returns `true` if the relative path looks like a Next.js Pages Router API
/// route: `pages/api/**/*.{ts,tsx,js}` (excluding `_app`, `_document`,
/// index files used as layout helpers, and `node_modules`).
fn is_pages_router_route(rel: &str) -> bool {
    if !rel.starts_with("pages/api/") && rel != "pages/api" {
        return false;
    }
    let Some(idx) = rel.rfind('/') else { return false; };
    let filename = &rel[idx + 1..];

    // Must end in .ts, .tsx, or .js — not test files
    let is_api_file = filename.ends_with(".ts")
        || filename.ends_with(".tsx")
        || filename.ends_with(".js");

    let is_noise = filename.starts_with('_')
        || filename.contains(".test.")
        || filename.contains(".spec.")
        || filename.contains(".d.ts");

    is_api_file && !is_noise
}

// ---------------------------------------------------------------------------
// App Router
// ---------------------------------------------------------------------------

fn process_app_router_file(
    root_slug: &str,
    root_path: &Path,
    rel_path: &Path,
    rel_forward: &str,
    abs_path: &Path,
    fn_index: &std::collections::HashMap<(PathBuf, String), NodeId>,
    result: &mut NextjsRoutingResult,
) {
    let http_path = derive_app_router_path(rel_forward);
    if http_path.is_empty() {
        return;
    }

    // Read file to detect exported HTTP-method functions.
    let content = match std::fs::read_to_string(abs_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "nextjs_routing_pass: failed to read {}: {}",
                abs_path.display(),
                e
            );
            return;
        }
    };

    let methods = find_exported_http_methods(&content);

    // If no recognized methods found, emit a catch-all ANY endpoint so the
    // route is at least visible in the graph.
    let effective_methods: Vec<&str> = if methods.is_empty() {
        vec!["ANY"]
    } else {
        methods.iter().map(|s| s.as_str()).collect()
    };

    for method in effective_methods {
        let name = format!("{} {}", method, http_path);
        let mut metadata = BTreeMap::new();
        metadata.insert("http_method".to_string(), method.to_string());
        metadata.insert("http_path".to_string(), http_path.clone());
        metadata.insert("source_convention".to_string(), "nextjs_app_router".to_string());

        let endpoint_id = NodeId {
            root: root_slug.to_string(),
            file: rel_path.to_path_buf(),
            name: name.clone(),
            kind: NodeKind::ApiEndpoint,
        };

        // Emit Implements edge to the handler function if it exists.
        if method != "ANY" {
            if let Some(handler_id) = fn_index.get(&(rel_path.to_path_buf(), method.to_string())) {
                result.edges.push(Edge {
                    from: endpoint_id.clone(),
                    to: handler_id.clone(),
                    kind: EdgeKind::Implements,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                });
            }
        }

        // Determine representative line numbers from the file (approximate).
        // Use line 1 since we don't have tree-sitter positions here.
        let (line_start, line_end) = find_method_lines(&content, method);

        result.nodes.push(Node {
            id: endpoint_id,
            language: language_from_path(abs_path),
            line_start,
            line_end,
            signature: format!("[nextjs_app_router] {} {}", method, http_path),
            body: String::new(),
            metadata,
            source: ExtractionSource::TreeSitter,
        });
    }
}

/// Derive the HTTP path from an App Router route file path.
///
/// Example:
/// `app/api/payments/route.ts` → `/payments`
/// `app/api/users/[id]/route.ts` → `/users/{id}`
/// `app/api/v2/items/[...slug]/route.ts` → `/v2/items/{slug}`
fn derive_app_router_path(rel: &str) -> String {
    // Strip "app/api" prefix (handle optional leading component like "src/app/api")
    let after_api = if let Some(s) = rel.strip_prefix("app/api/") {
        s
    } else if rel == "app/api/route.ts"
        || rel == "app/api/route.tsx"
        || rel == "app/api/route.js"
        || rel == "app/api/route.jsx"
    {
        // route directly under app/api — no sub-path
        return "/".to_string();
    } else if let Some(s) = strip_prefix_ci(rel, "app/api/") {
        s
    } else {
        return String::new();
    };

    // Remove the trailing "route.{ext}" filename
    let dir_part = match after_api.rfind('/') {
        Some(idx) => &after_api[..idx],
        None => {
            // The file IS directly under app/api/ (e.g., app/api/route.ts)
            return "/".to_string();
        }
    };

    if dir_part.is_empty() {
        return "/".to_string();
    }

    // Convert directory segments: [id] → {id}, [...slug] → {slug}
    let segments: Vec<String> = dir_part
        .split('/')
        .map(|seg| convert_nextjs_segment(seg))
        .collect();

    format!("/{}", segments.join("/"))
}

/// Case-insensitive strip_prefix helper (used for src/App/api variants).
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.to_lowercase().starts_with(&prefix.to_lowercase()) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Pages Router
// ---------------------------------------------------------------------------

fn process_pages_router_file(
    root_slug: &str,
    rel_path: &Path,
    rel_forward: &str,
    abs_path: &Path,
    fn_index: &std::collections::HashMap<(PathBuf, String), NodeId>,
    result: &mut NextjsRoutingResult,
) {
    let http_path = derive_pages_router_path(rel_forward);
    if http_path.is_empty() {
        return;
    }

    let method = "ANY";
    let name = format!("{} {}", method, http_path);
    let mut metadata = BTreeMap::new();
    metadata.insert("http_method".to_string(), method.to_string());
    metadata.insert("http_path".to_string(), http_path.clone());
    metadata.insert("source_convention".to_string(), "nextjs_pages_router".to_string());

    let endpoint_id = NodeId {
        root: root_slug.to_string(),
        file: rel_path.to_path_buf(),
        name: name.clone(),
        kind: NodeKind::ApiEndpoint,
    };

    // Emit Implements edge to the default export handler if it exists.
    // Next.js Pages Router default export is named "default" or the handler
    // function. We try common patterns: "default", "handler".
    for handler_name in &["default", "handler"] {
        if let Some(handler_id) =
            fn_index.get(&(rel_path.to_path_buf(), handler_name.to_string()))
        {
            result.edges.push(Edge {
                from: endpoint_id.clone(),
                to: handler_id.clone(),
                kind: EdgeKind::Implements,
                source: ExtractionSource::TreeSitter,
                confidence: Confidence::Detected,
            });
            break; // only link one handler
        }
    }

    result.nodes.push(Node {
        id: endpoint_id,
        language: language_from_path(abs_path),
        line_start: 1,
        line_end: 1,
        signature: format!("[nextjs_pages_router] {} {}", method, http_path),
        body: String::new(),
        metadata,
        source: ExtractionSource::TreeSitter,
    });
}

/// Derive the HTTP path from a Pages Router API file path.
///
/// Example:
/// `pages/api/payments.ts` → `/api/payments`
/// `pages/api/users/[id].ts` → `/api/users/{id}`
fn derive_pages_router_path(rel: &str) -> String {
    // Strip "pages/" prefix — keep "api/" since Pages Router paths include it
    let after_pages = match rel.strip_prefix("pages/") {
        Some(s) => s,
        None => return String::new(),
    };

    // Remove extension
    let without_ext = strip_ts_extension(after_pages);
    if without_ext.is_empty() {
        return String::new();
    }

    // Handle "index" files: pages/api/index.ts → /api
    let path_part = if without_ext.ends_with("/index") {
        &without_ext[..without_ext.len() - "/index".len()]
    } else {
        &without_ext
    };

    // Convert dynamic segments
    let segments: Vec<String> = path_part
        .split('/')
        .map(|seg| convert_nextjs_segment(seg))
        .collect();

    format!("/{}", segments.join("/"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a Next.js path segment to RFC-style `{param}` form.
///
/// | Input | Output |
/// |-------|--------|
/// | `[id]` | `{id}` |
/// | `[...slug]` | `{slug}` |
/// | `[[...slug]]` | `{slug}` |
/// | `payments` | `payments` |
fn convert_nextjs_segment(seg: &str) -> String {
    // Optional catch-all: [[...slug]]
    if seg.starts_with("[[...") && seg.ends_with("]]") {
        let inner = &seg[5..seg.len() - 2];
        return format!("{{{}}}", inner);
    }
    // Catch-all: [...slug]
    if seg.starts_with("[...") && seg.ends_with(']') {
        let inner = &seg[4..seg.len() - 1];
        return format!("{{{}}}", inner);
    }
    // Dynamic: [id]
    if seg.starts_with('[') && seg.ends_with(']') {
        let inner = &seg[1..seg.len() - 1];
        return format!("{{{}}}", inner);
    }
    seg.to_string()
}

/// Strip common TypeScript/JavaScript file extensions.
fn strip_ts_extension(s: &str) -> &str {
    for ext in &[".tsx", ".ts", ".jsx", ".js"] {
        if let Some(stripped) = s.strip_suffix(ext) {
            return stripped;
        }
    }
    s
}

/// Detect exported HTTP-method functions in a TypeScript/JavaScript file.
///
/// Matches patterns like:
/// - `export function GET(`
/// - `export async function POST(`
/// - `export const DELETE =`
/// - `export const PATCH: NextRequest =`
///
/// Returns a deduplicated list of HTTP methods found.
pub fn find_exported_http_methods(content: &str) -> Vec<String> {
    const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];

    let mut found: Vec<String> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("export") {
            continue;
        }
        for method in HTTP_METHODS {
            if found.iter().any(|m| m == method) {
                continue; // already found
            }
            // Match: export function METHOD, export async function METHOD, export const METHOD
            if trimmed.contains(&format!("function {}", method))
                || trimmed.contains(&format!("const {} ", method))
                || trimmed.contains(&format!("const {}=", method))
                || trimmed.contains(&format!("const {}:", method))
            {
                found.push(method.to_string());
            }
        }
    }
    found
}

/// Find the approximate start/end line numbers (1-indexed) of an exported
/// HTTP method function in a file.  Returns (1, 1) if not found.
fn find_method_lines(content: &str, method: &str) -> (usize, usize) {
    if method == "ANY" {
        return (1, 1);
    }
    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with("export") {
            continue;
        }
        if trimmed.contains(&format!("function {}", method))
            || trimmed.contains(&format!("const {} ", method))
            || trimmed.contains(&format!("const {}=", method))
            || trimmed.contains(&format!("const {}:", method))
        {
            let line_num = idx + 1;
            return (line_num, line_num);
        }
    }
    (1, 1)
}

/// Infer language name from file extension.
fn language_from_path(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ts") | Some("tsx") => "typescript".to_string(),
        Some("js") | Some("jsx") => "javascript".to_string(),
        _ => "typescript".to_string(), // default for Next.js
    }
}

// ---------------------------------------------------------------------------
// Filesystem walker
// ---------------------------------------------------------------------------

/// Walk `root` recursively, calling `callback` for each regular file.
/// Skips well-known noise directories: `node_modules`, `.git`, `dist`,
/// `build`, `.next`, `coverage`, `target`, `.oh`, `.claude`.
fn walk_for_nextjs(root: &Path, callback: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(root) else { return; };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if path.is_dir() {
            if should_skip_dir(name) {
                continue;
            }
            walk_for_nextjs(&path, callback);
        } else if path.is_file() {
            callback(&path);
        }
    }
}

fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | ".git"
            | ".svn"
            | "dist"
            | "build"
            | "out"
            | ".next"
            | "coverage"
            | "target"
            | ".oh"
            | ".claude"
            | "__pycache__"
            | ".pytest_cache"
            | ".ruff_cache"
            | "vendor"
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // -----------------------------------------------------------------------
    // Path derivation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_app_router_simple_path() {
        assert_eq!(
            derive_app_router_path("app/api/payments/route.ts"),
            "/payments"
        );
    }

    #[test]
    fn test_app_router_nested_path() {
        assert_eq!(
            derive_app_router_path("app/api/users/profile/route.ts"),
            "/users/profile"
        );
    }

    #[test]
    fn test_app_router_dynamic_segment() {
        assert_eq!(
            derive_app_router_path("app/api/users/[id]/route.ts"),
            "/users/{id}"
        );
    }

    #[test]
    fn test_app_router_catch_all_segment() {
        assert_eq!(
            derive_app_router_path("app/api/files/[...slug]/route.ts"),
            "/files/{slug}"
        );
    }

    #[test]
    fn test_app_router_optional_catch_all() {
        assert_eq!(
            derive_app_router_path("app/api/files/[[...slug]]/route.ts"),
            "/files/{slug}"
        );
    }

    #[test]
    fn test_app_router_root_route() {
        assert_eq!(derive_app_router_path("app/api/route.ts"), "/");
    }

    #[test]
    fn test_pages_router_simple_path() {
        assert_eq!(
            derive_pages_router_path("pages/api/payments.ts"),
            "/api/payments"
        );
    }

    #[test]
    fn test_pages_router_nested_path() {
        assert_eq!(
            derive_pages_router_path("pages/api/users/profile.ts"),
            "/api/users/profile"
        );
    }

    #[test]
    fn test_pages_router_dynamic_segment() {
        assert_eq!(
            derive_pages_router_path("pages/api/users/[id].ts"),
            "/api/users/{id}"
        );
    }

    #[test]
    fn test_pages_router_index_file() {
        assert_eq!(
            derive_pages_router_path("pages/api/index.ts"),
            "/api"
        );
    }

    // -----------------------------------------------------------------------
    // Route detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_app_router_route() {
        assert!(is_app_router_route("app/api/payments/route.ts"));
        assert!(is_app_router_route("app/api/payments/route.tsx"));
        assert!(is_app_router_route("app/api/payments/route.js"));
        assert!(is_app_router_route("app/api/route.ts"));
        // NOT routes
        assert!(!is_app_router_route("app/api/payments/page.tsx"));
        assert!(!is_app_router_route("src/api/payments.ts"));
    }

    #[test]
    fn test_is_pages_router_route() {
        assert!(is_pages_router_route("pages/api/payments.ts"));
        assert!(is_pages_router_route("pages/api/payments.js"));
        assert!(is_pages_router_route("pages/api/users/[id].ts"));
        // NOT routes
        assert!(!is_pages_router_route("pages/api/_app.ts"));
        assert!(!is_pages_router_route("pages/api/payments.test.ts"));
        assert!(!is_pages_router_route("pages/api/types.d.ts"));
        assert!(!is_pages_router_route("src/pages/api/payments.ts"));
    }

    // -----------------------------------------------------------------------
    // find_exported_http_methods tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_exported_http_methods_function_style() {
        let content = r#"
import { NextRequest } from 'next/server'

export async function GET(request: NextRequest) {
    return Response.json({ data: [] })
}

export async function POST(request: NextRequest) {
    const body = await request.json()
    return Response.json({ created: true })
}
"#;
        let methods = find_exported_http_methods(content);
        assert!(methods.contains(&"GET".to_string()), "should find GET");
        assert!(methods.contains(&"POST".to_string()), "should find POST");
        assert_eq!(methods.len(), 2);
    }

    #[test]
    fn test_find_exported_http_methods_const_style() {
        let content = r#"
import { NextResponse } from 'next/server'

export const DELETE = async (req: Request) => {
    return NextResponse.json({ deleted: true })
}
"#;
        let methods = find_exported_http_methods(content);
        assert!(methods.contains(&"DELETE".to_string()), "should find DELETE");
    }

    #[test]
    fn test_find_exported_http_methods_none() {
        let content = r#"
// This file has no HTTP method exports
export function helper() {}
"#;
        let methods = find_exported_http_methods(content);
        assert!(methods.is_empty(), "should find no HTTP methods");
    }

    // -----------------------------------------------------------------------
    // Segment conversion tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_convert_dynamic_segment() {
        assert_eq!(convert_nextjs_segment("[id]"), "{id}");
        assert_eq!(convert_nextjs_segment("[userId]"), "{userId}");
    }

    #[test]
    fn test_convert_catch_all_segment() {
        assert_eq!(convert_nextjs_segment("[...slug]"), "{slug}");
        assert_eq!(convert_nextjs_segment("[[...path]]"), "{path}");
    }

    #[test]
    fn test_convert_static_segment() {
        assert_eq!(convert_nextjs_segment("payments"), "payments");
        assert_eq!(convert_nextjs_segment("api"), "api");
    }

    // -----------------------------------------------------------------------
    // Integration: nextjs_routing_pass with temp filesystem
    // -----------------------------------------------------------------------

    #[test]
    fn test_nextjs_routing_pass_app_router() {
        use std::fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();

        // Create App Router route file
        let route_dir = root.join("app").join("api").join("payments");
        fs::create_dir_all(&route_dir).unwrap();
        fs::write(
            route_dir.join("route.ts"),
            "export async function GET() { return Response.json([]) }\nexport async function POST() { }\n",
        ).unwrap();

        // Run the pass with no existing Function nodes
        let roots = vec![("test".to_string(), root)];
        let result = nextjs_routing_pass(&roots, &[]);

        // Should produce 2 ApiEndpoint nodes (GET and POST)
        let endpoints: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();

        assert_eq!(endpoints.len(), 2, "expected 2 ApiEndpoint nodes, got: {:?}", endpoints);

        let paths: Vec<_> = endpoints
            .iter()
            .map(|n| n.metadata.get("http_path").cloned().unwrap_or_default())
            .collect();
        assert!(paths.iter().all(|p| p == "/payments"), "all should have path /payments");

        let methods: std::collections::HashSet<_> = endpoints
            .iter()
            .map(|n| n.metadata.get("http_method").cloned().unwrap_or_default())
            .collect();
        assert!(methods.contains("GET"), "should have GET");
        assert!(methods.contains("POST"), "should have POST");
    }

    #[test]
    fn test_nextjs_routing_pass_pages_router() {
        use std::fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();

        // Create Pages Router route file
        let api_dir = root.join("pages").join("api");
        fs::create_dir_all(&api_dir).unwrap();
        fs::write(
            api_dir.join("users.ts"),
            "export default function handler(req, res) { res.json([]) }\n",
        ).unwrap();

        let roots = vec![("test".to_string(), root)];
        let result = nextjs_routing_pass(&roots, &[]);

        let endpoints: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();

        assert_eq!(endpoints.len(), 1, "expected 1 ApiEndpoint node");
        assert_eq!(
            endpoints[0].metadata.get("http_path").map(|s| s.as_str()),
            Some("/api/users")
        );
        assert_eq!(
            endpoints[0].metadata.get("http_method").map(|s| s.as_str()),
            Some("ANY")
        );
    }

    #[test]
    fn test_implements_edge_emitted_when_function_node_exists() {
        use std::fs;
        use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();

        let route_dir = root.join("app").join("api").join("items");
        std::fs::create_dir_all(&route_dir).unwrap();
        std::fs::write(
            route_dir.join("route.ts"),
            "export async function GET() {}\n",
        ).unwrap();

        // Create a matching Function node
        let handler_node = Node {
            id: NodeId {
                root: "test".to_string(),
                file: PathBuf::from("app/api/items/route.ts"),
                name: "GET".to_string(),
                kind: NodeKind::Function,
            },
            language: "typescript".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "export async function GET()".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let roots = vec![("test".to_string(), root)];
        let result = nextjs_routing_pass(&roots, &[handler_node]);

        let implements_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();

        assert_eq!(implements_edges.len(), 1, "should emit 1 Implements edge");
        assert_eq!(implements_edges[0].to.name, "GET");
    }

    #[test]
    fn test_node_modules_skipped() {
        use std::fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();

        // Put a route file inside node_modules — should be ignored
        let nm_dir = root
            .join("node_modules")
            .join("some-pkg")
            .join("app")
            .join("api")
            .join("payments");
        fs::create_dir_all(&nm_dir).unwrap();
        fs::write(nm_dir.join("route.ts"), "export async function GET() {}\n").unwrap();

        let roots = vec![("test".to_string(), root)];
        let result = nextjs_routing_pass(&roots, &[]);

        assert!(
            result.nodes.is_empty(),
            "node_modules routes should be ignored"
        );
    }
}
