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
//! ## App Router (`app/api/**/route.{ts,tsx,js,jsx}`)
//!
//! Per Next.js App Router conventions the `api/` segment is part of the URL:
//!
//! ```text
//! app/api/payments/route.ts    →  /api/payments   (exported GET, POST, …)
//! app/api/users/[id]/route.ts  →  /api/users/{id}  (exported GET, PUT, DELETE, …)
//! app/api/route.ts             →  /api             (root of API namespace)
//! ```
//!
//! One `ApiEndpoint` node is emitted per exported HTTP-method function found
//! in the file (`GET`, `POST`, `PUT`, `DELETE`, `PATCH`, `HEAD`, `OPTIONS`).
//! Exports can be inline (`export function GET(…)`, `export const GET = …`)
//! or re-exported (`export { GET }`, `export { handler as GET }`).
//! An `Implements` edge links each endpoint node to the corresponding
//! `Function` node already present in `existing_nodes`.
//!
//! ## Pages Router (`pages/api/**/*.{ts,tsx,js,jsx}`)
//!
//! ```text
//! pages/api/payments.ts       →  ANY /api/payments
//! pages/api/users/[id].ts     →  ANY /api/users/{id}
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
//! | App Router strip | `app/api/payments/route.ts` | `/api/payments` |
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

        // Strip optional leading "src/" for path derivation (the stripped form
        // is used for both detection AND HTTP path derivation, but the original
        // rel_path is used as the NodeId file path so the node matches tree-sitter
        // extracted nodes which use the original relative path).
        let rel_for_matching = rel_forward.strip_prefix("src/").unwrap_or(&rel_forward);

        if is_app_router_route(&rel_forward) {
            process_app_router_file(
                root_slug,
                root_path,
                rel_path,
                rel_for_matching,
                abs_path,
                &fn_index,
                &mut result,
            );
        } else if is_pages_router_route(&rel_forward) {
            process_pages_router_file(
                root_slug,
                rel_path,
                rel_for_matching,
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
/// route: `app/api/**/route.{ts,tsx,js,jsx}` (or `src/app/api/…`).
fn is_app_router_route(rel: &str) -> bool {
    // Strip optional leading "src/" prefix (common Next.js layout)
    let rel = rel.strip_prefix("src/").unwrap_or(rel);

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
/// route: `pages/api/**/*.{ts,tsx,js,jsx}` (or `src/pages/api/…`), excluding
/// `_app`, `_document`, test files, and `.d.ts` declaration files.
fn is_pages_router_route(rel: &str) -> bool {
    // Strip optional leading "src/" prefix
    let rel = rel.strip_prefix("src/").unwrap_or(rel);

    if !rel.starts_with("pages/api/") && rel != "pages/api" {
        return false;
    }
    let Some(idx) = rel.rfind('/') else { return false; };
    let filename = &rel[idx + 1..];

    // Must end in .ts, .tsx, .js, or .jsx — not test files
    // .jsx is rare for API routes but included for consistency with App Router.
    let is_api_file = filename.ends_with(".ts")
        || filename.ends_with(".tsx")
        || filename.ends_with(".js")
        || filename.ends_with(".jsx");

    let is_noise = filename.starts_with('_')
        || filename.contains(".test.")
        || filename.contains(".spec.")
        || filename.ends_with(".d.ts");

    is_api_file && !is_noise
}

// ---------------------------------------------------------------------------
// App Router
// ---------------------------------------------------------------------------

fn process_app_router_file(
    root_slug: &str,
    _root_path: &Path,
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

    let bindings = find_exported_http_methods(&content);

    // If no recognized methods found, emit a catch-all ANY endpoint so the
    // route is at least visible in the graph even if we can't parse methods.
    if bindings.is_empty() {
        let name = format!("ANY {}", http_path);
        let mut metadata = BTreeMap::new();
        metadata.insert("http_method".to_string(), "ANY".to_string());
        metadata.insert("http_path".to_string(), http_path.clone());
        metadata.insert("source_convention".to_string(), "nextjs_app_router".to_string());
        result.nodes.push(Node {
            id: NodeId {
                root: root_slug.to_string(),
                file: rel_path.to_path_buf(),
                name,
                kind: NodeKind::ApiEndpoint,
            },
            language: language_from_path(abs_path),
            line_start: 1,
            line_end: 1,
            signature: format!("[nextjs_app_router] ANY {}", http_path),
            body: String::new(),
            metadata,
            source: ExtractionSource::TreeSitter,
        });
        return;
    }

    for binding in &bindings {
        let method = &binding.http_method;
        let name = format!("{} {}", method, http_path);
        let mut metadata = BTreeMap::new();
        metadata.insert("http_method".to_string(), method.clone());
        metadata.insert("http_path".to_string(), http_path.clone());
        metadata.insert("source_convention".to_string(), "nextjs_app_router".to_string());

        let endpoint_id = NodeId {
            root: root_slug.to_string(),
            file: rel_path.to_path_buf(),
            name: name.clone(),
            kind: NodeKind::ApiEndpoint,
        };

        // Emit Implements edge to the handler function.
        // Use `local_name` so that aliased re-exports like `export { handler as GET }`
        // correctly link to the `handler` Function node (not a non-existent `GET` node).
        if let Some(handler_id) = fn_index.get(&(rel_path.to_path_buf(), binding.local_name.clone())) {
            result.edges.push(Edge {
                from: endpoint_id.clone(),
                to: handler_id.clone(),
                kind: EdgeKind::Implements,
                source: ExtractionSource::TreeSitter,
                confidence: Confidence::Detected,
            });
        }

        result.nodes.push(Node {
            id: endpoint_id,
            language: language_from_path(abs_path),
            line_start: binding.line,
            line_end: binding.line,
            signature: format!("[nextjs_app_router] {} {}", method, http_path),
            body: String::new(),
            metadata,
            source: ExtractionSource::TreeSitter,
        });
    }
}

/// Derive the HTTP path from an App Router route file path.
///
/// Per Next.js App Router routing conventions the URL path mirrors the directory
/// structure under `app/` — the `api` segment IS part of the URL path.
///
/// | File path | HTTP path |
/// |-----------|-----------|
/// | `app/api/route.ts` | `/api` |
/// | `app/api/payments/route.ts` | `/api/payments` |
/// | `app/api/users/[id]/route.ts` | `/api/users/{id}` |
/// | `app/api/v2/items/[...slug]/route.ts` | `/api/v2/items/{slug}` |
///
/// `rel` must already have any leading `src/` stripped by the caller
/// (i.e. it is the path as seen from the root `app/` directory).
fn derive_app_router_path(rel: &str) -> String {
    // Strip only "app/" prefix — "api/" and all subdirectory segments stay as
    // part of the URL path.
    let after_app = if let Some(s) = rel.strip_prefix("app/") {
        s
    } else if let Some(s) = strip_prefix_ci(rel, "app/") {
        s
    } else {
        return String::new();
    };

    // Remove the trailing "route.{ext}" filename.
    let dir_part = match after_app.rfind('/') {
        Some(idx) => &after_app[..idx],
        None => {
            // Path like "app/route.ts" — directly under app/ with no subdir.
            // This is unusual but map to "/" to be safe.
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
///
/// This helper is only ever called with ASCII prefixes (file path segments like
/// "app/api/"), so slicing by `prefix.len()` bytes is safe. Non-ASCII file paths
/// would not match a Next.js app/api/ prefix pattern anyway.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let lower_s = s.to_lowercase();
    let lower_prefix = prefix.to_lowercase();
    if lower_s.starts_with(&lower_prefix) {
        // Safe: both s and prefix are ASCII file path segments.
        // prefix.len() == lower_prefix.len() for ASCII, so we can slice
        // the original string by the prefix byte length.
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

/// A binding between an exported HTTP method name and the local function name.
///
/// | Export form | `http_method` | `local_name` | `line` |
/// |-------------|--------------|-------------|--------|
/// | `export function GET() {}` | `"GET"` | `"GET"` | 1-indexed line of export |
/// | `export const GET = …` | `"GET"` | `"GET"` | same |
/// | `export { GET }` | `"GET"` | `"GET"` | same |
/// | `export { handler as GET }` | `"GET"` | `"handler"` | same |
#[derive(Debug, Clone, PartialEq)]
pub struct MethodBinding {
    /// HTTP method exported from the file (e.g. `"GET"`, `"POST"`).
    pub http_method: String,
    /// Local identifier to look up in the tree-sitter node index.
    /// For inline exports this equals `http_method`.
    /// For `export { handler as GET }` this is `"handler"`.
    pub local_name: String,
    /// 1-indexed line number of the export statement.
    pub line: usize,
}

/// Scan `content` for HTTP-method exports and return one `MethodBinding` per
/// unique method found.
///
/// Supported forms:
/// - `export function GET(…)`
/// - `export async function POST(…)`
/// - `export const DELETE = …`
/// - `export const PATCH: NextRequestHandler = …`
/// - `export { GET }` — direct single-line re-export
/// - `export { handler as GET, handler as POST }` — aliased single-line
/// - Multiline `export {\n  handler as GET,\n  handler as POST\n}` blocks
///
/// Uses word-boundary checks to prevent `GETTER` matching `GET`, etc.
///
/// Returns deduplicated bindings in source order.
pub fn find_exported_http_methods(content: &str) -> Vec<MethodBinding> {
    const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];

    let mut bindings: Vec<MethodBinding> = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        let line_num = i + 1; // 1-indexed

        if trimmed.starts_with("export") {
            // --- Inline: `export function GET` / `export async function GET`
            // --- Inline: `export const GET = …` / `export const GET: …`
            for &method in HTTP_METHODS {
                if bindings.iter().any(|b| b.http_method == method) {
                    continue;
                }
                if inline_exports_http_method(trimmed, method) {
                    bindings.push(MethodBinding {
                        http_method: method.to_string(),
                        local_name: method.to_string(),
                        line: line_num,
                    });
                }
            }

            // --- Re-export block: `export { … }` (single or multiline)
            if trimmed.contains('{') {
                // Accumulate lines until we find the closing `}`.
                let mut block = String::new();
                let block_start = line_num;
                let mut j = i;
                while j < lines.len() {
                    block.push_str(lines[j]);
                    block.push('\n');
                    if lines[j].contains('}') {
                        break;
                    }
                    j += 1;
                }
                // Extract the inner content of `{ … }`
                if let (Some(open), Some(close)) = (block.find('{'), block.rfind('}')) {
                    if open < close {
                        let inner = &block[open + 1..close];
                        for item in inner.split(',') {
                            let item = item.trim().trim_end_matches(',').trim();
                            if item.is_empty() {
                                continue;
                            }
                            // `handler as METHOD` or just `METHOD`
                            let (local, exported) = if let Some(pos) = item.find(" as ") {
                                (item[..pos].trim(), item[pos + 4..].trim())
                            } else {
                                (item, item)
                            };
                            // Check if exported name is an HTTP method
                            if HTTP_METHODS.contains(&exported)
                                && !bindings.iter().any(|b| b.http_method == exported)
                            {
                                bindings.push(MethodBinding {
                                    http_method: exported.to_string(),
                                    local_name: local.to_string(),
                                    line: block_start,
                                });
                            }
                        }
                    }
                }
            }
        }
        i += 1;
    }

    bindings
}

/// Returns `true` if a single `line` declares an inline export for the given
/// HTTP `method` (function or const form) with proper word-boundary checks.
///
/// Does NOT handle re-export blocks — those are handled by the multiline
/// collector in `find_exported_http_methods`.
fn inline_exports_http_method(line: &str, method: &str) -> bool {
    // `export function METHOD` / `export async function METHOD`
    let fn_pattern = format!("function {}", method);
    if let Some(pos) = line.find(&fn_pattern) {
        let after = &line[pos + fn_pattern.len()..];
        let next = after.chars().next();
        if matches!(next, Some('(') | Some(' ') | Some('\t') | None) {
            return true;
        }
    }

    // `export const METHOD =` / `export const METHOD:` — even if line has `{` later
    // (e.g., `export const DELETE = async (req) => { … }`)
    let const_pattern = format!("const {}", method);
    if let Some(pos) = line.find(&const_pattern) {
        let after = &line[pos + const_pattern.len()..];
        let next = after.chars().next();
        if matches!(next, Some(' ') | Some('=') | Some(':') | None) {
            return true;
        }
    }

    false
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
///
/// Symlinks are intentionally skipped for both files and directories to avoid
/// infinite recursion from directory link cycles and to prevent accidentally
/// walking large external trees linked into the project.
fn walk_for_nextjs(root: &Path, callback: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(root) else { return; };
    for entry in entries.flatten() {
        // Use file_type() rather than path.is_dir()/is_file() so that symlinks
        // are not followed — path.is_dir() follows symlinks and can recurse into
        // cycles or large external trees.
        let Ok(ft) = entry.file_type() else { continue; };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if ft.is_dir() {
            if should_skip_dir(name) {
                continue;
            }
            walk_for_nextjs(&path, callback);
        } else if ft.is_file() {
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
        // app/api/ is part of the URL path per Next.js App Router conventions
        assert_eq!(
            derive_app_router_path("app/api/payments/route.ts"),
            "/api/payments"
        );
    }

    #[test]
    fn test_app_router_nested_path() {
        assert_eq!(
            derive_app_router_path("app/api/users/profile/route.ts"),
            "/api/users/profile"
        );
    }

    #[test]
    fn test_app_router_dynamic_segment() {
        assert_eq!(
            derive_app_router_path("app/api/users/[id]/route.ts"),
            "/api/users/{id}"
        );
    }

    #[test]
    fn test_app_router_catch_all_segment() {
        assert_eq!(
            derive_app_router_path("app/api/files/[...slug]/route.ts"),
            "/api/files/{slug}"
        );
    }

    #[test]
    fn test_app_router_optional_catch_all() {
        assert_eq!(
            derive_app_router_path("app/api/files/[[...slug]]/route.ts"),
            "/api/files/{slug}"
        );
    }

    #[test]
    fn test_app_router_root_route() {
        // app/api/route.ts → /api (the root of the API namespace)
        assert_eq!(derive_app_router_path("app/api/route.ts"), "/api");
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
        // With src/ prefix (common Next.js layout)
        assert!(is_app_router_route("src/app/api/payments/route.ts"));
        assert!(is_app_router_route("src/app/api/route.ts"));
        // NOT routes
        assert!(!is_app_router_route("app/api/payments/page.tsx"));
        assert!(!is_app_router_route("src/api/payments.ts"));
    }

    #[test]
    fn test_is_pages_router_route() {
        assert!(is_pages_router_route("pages/api/payments.ts"));
        assert!(is_pages_router_route("pages/api/payments.js"));
        assert!(is_pages_router_route("pages/api/payments.jsx"));
        assert!(is_pages_router_route("pages/api/users/[id].ts"));
        // With src/ prefix
        assert!(is_pages_router_route("src/pages/api/payments.ts"));
        // NOT routes
        assert!(!is_pages_router_route("pages/api/_app.ts"));
        assert!(!is_pages_router_route("pages/api/payments.test.ts"));
        assert!(!is_pages_router_route("pages/api/types.d.ts"));
    }

    // -----------------------------------------------------------------------
    // find_exported_http_methods tests
    // -----------------------------------------------------------------------

    fn methods_in(bindings: &[MethodBinding]) -> Vec<&str> {
        bindings.iter().map(|b| b.http_method.as_str()).collect()
    }

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
        let bindings = find_exported_http_methods(content);
        let methods = methods_in(&bindings);
        assert!(methods.contains(&"GET"), "should find GET");
        assert!(methods.contains(&"POST"), "should find POST");
        assert_eq!(bindings.len(), 2);
        // Inline exports: local_name == http_method
        assert!(bindings.iter().all(|b| b.local_name == b.http_method));
    }

    #[test]
    fn test_find_exported_http_methods_const_style() {
        let content = r#"
import { NextResponse } from 'next/server'

export const DELETE = async (req: Request) => {
    return NextResponse.json({ deleted: true })
}
"#;
        let bindings = find_exported_http_methods(content);
        assert!(methods_in(&bindings).contains(&"DELETE"), "should find DELETE");
    }

    #[test]
    fn test_find_exported_http_methods_none() {
        let content = r#"
// This file has no HTTP method exports
export function helper() {}
"#;
        let bindings = find_exported_http_methods(content);
        assert!(bindings.is_empty(), "should find no HTTP methods");
    }

    #[test]
    fn test_find_exported_http_methods_no_false_positives() {
        // GETTER should NOT match GET; DELETED should NOT match DELETE
        let content = r#"
export function GETTER() {}
export const DELETED = async () => {}
export const POSTFIX = "something"
"#;
        let bindings = find_exported_http_methods(content);
        let methods = methods_in(&bindings);
        assert!(!methods.contains(&"GET"), "GETTER should not match GET, got: {:?}", methods);
        assert!(!methods.contains(&"DELETE"), "DELETED should not match DELETE, got: {:?}", methods);
        assert!(!methods.contains(&"POST"), "POSTFIX should not match POST, got: {:?}", methods);
    }

    #[test]
    fn test_find_exported_http_methods_reexport_syntax() {
        // Direct re-export: `export { GET }`
        let content1 = "export { GET }\n";
        let b1 = find_exported_http_methods(content1);
        assert!(methods_in(&b1).contains(&"GET"), "should detect export {{ GET }}");
        // local_name == http_method for direct re-export
        assert_eq!(b1[0].local_name, "GET");

        // Aliased re-export: `export { handler as POST }`
        let content2 = "export { handler as POST }\n";
        let b2 = find_exported_http_methods(content2);
        assert!(methods_in(&b2).contains(&"POST"), "should detect export {{ handler as POST }}");
        // local_name should be "handler"
        assert_eq!(b2[0].local_name, "handler", "local_name should be the original function name");

        // Multiple re-exports: `export { GET, POST }`
        let content3 = "export { GET, POST }\n";
        let b3 = find_exported_http_methods(content3);
        assert!(methods_in(&b3).contains(&"GET"), "should detect GET in multi-export");
        assert!(methods_in(&b3).contains(&"POST"), "should detect POST in multi-export");

        // Re-export with FROM: `export { GET } from './handler'`
        let content4 = "export { GET } from './handler'\n";
        let b4 = find_exported_http_methods(content4);
        assert!(methods_in(&b4).contains(&"GET"), "should detect export {{ GET }} from ...");

        // Multiline export block
        let content5 = "export {\n  handler as GET,\n  handler as POST\n}\n";
        let b5 = find_exported_http_methods(content5);
        assert!(methods_in(&b5).contains(&"GET"), "should detect GET from multiline block");
        assert!(methods_in(&b5).contains(&"POST"), "should detect POST from multiline block");
        // local_name should be "handler" for aliased exports
        for b in &b5 {
            assert_eq!(b.local_name, "handler", "local_name for aliased multiline should be 'handler'");
        }
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
        assert!(paths.iter().all(|p| p == "/api/payments"), "all should have path /api/payments");

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
    fn test_implements_edge_aliased_reexport() {
        // `export { handler as GET }` — the Implements edge must link to `handler`, not `GET`
        use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};
        use std::collections::BTreeMap;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();

        let route_dir = root.join("app").join("api").join("orders");
        std::fs::create_dir_all(&route_dir).unwrap();
        std::fs::write(
            route_dir.join("route.ts"),
            "async function handler() {}\nexport { handler as GET }\n",
        ).unwrap();

        // Tree-sitter would produce a Function node named "handler"
        let handler_node = Node {
            id: NodeId {
                root: "test".to_string(),
                file: PathBuf::from("app/api/orders/route.ts"),
                name: "handler".to_string(),
                kind: NodeKind::Function,
            },
            language: "typescript".to_string(),
            line_start: 1,
            line_end: 1,
            signature: "async function handler()".to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let roots = vec![("test".to_string(), root)];
        let result = nextjs_routing_pass(&roots, &[handler_node]);

        let endpoints: Vec<_> = result.nodes.iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint).collect();
        assert_eq!(endpoints.len(), 1, "should emit 1 ApiEndpoint node");
        assert_eq!(endpoints[0].id.name, "GET /api/orders");

        let implements_edges: Vec<_> = result.edges.iter()
            .filter(|e| e.kind == EdgeKind::Implements).collect();
        assert_eq!(implements_edges.len(), 1, "should emit 1 Implements edge");
        assert_eq!(implements_edges[0].to.name, "handler",
            "Implements edge should link to 'handler' (local_name), not 'GET'");
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

    // -----------------------------------------------------------------------
    // Adversarial tests (seeded from /ship dissent findings)
    // -----------------------------------------------------------------------

    /// Adversarial: symlink to a directory must not be followed.
    /// If walk_for_nextjs followed symlinks, this would recurse and panic or
    /// produce duplicate results. Before the fix (Critical CodeRabbit finding),
    /// path.is_dir() followed symlinks; now we use entry.file_type().is_symlink().
    #[cfg(unix)]
    #[test]
    fn test_walk_does_not_follow_symlinked_directory() {
        use std::fs;
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();

        // Create a real route file
        let route_dir = root.join("app").join("api").join("real");
        fs::create_dir_all(&route_dir).unwrap();
        fs::write(route_dir.join("route.ts"), "export async function GET() {}\n").unwrap();

        // Create a symlink loop: root/loop_link → root (infinite recursion if followed)
        let loop_link = root.join("loop_link");
        symlink(&root, &loop_link).expect("failed to create symlink loop");

        // Should NOT recurse infinitely; should find exactly 1 ApiEndpoint
        let roots = vec![("test".to_string(), root)];
        let result = nextjs_routing_pass(&roots, &[]);

        let endpoints: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert_eq!(
            endpoints.len(),
            1,
            "should find exactly 1 ApiEndpoint, not recurse through symlink: {:?}",
            endpoints
        );
    }

    /// Adversarial: Next.js 13+ parallel routes use `@folder` notation.
    /// These are NOT API routes and must not produce ApiEndpoint nodes.
    #[test]
    fn test_parallel_routes_not_detected_as_api() {
        use std::fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();

        // Next.js parallel route: app/@dashboard/api/payments/route.ts
        // This is NOT an API route file in the conventional sense — the `@dashboard`
        // slot means it renders into a layout slot, not a standalone HTTP handler.
        let route_dir = root.join("app").join("@dashboard").join("api").join("payments");
        fs::create_dir_all(&route_dir).unwrap();
        fs::write(route_dir.join("route.ts"), "export async function GET() {}\n").unwrap();

        let roots = vec![("test".to_string(), root)];
        let result = nextjs_routing_pass(&roots, &[]);

        // The path starts with app/@dashboard/ not app/api/ — is_app_router_route checks
        // that the directory starts with app/api/, so parallel routes should be skipped.
        // This is the INTENDED behaviour: we only detect conventional API routes.
        let endpoints: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        // Parallel routes under @slot are not detected — document the boundary clearly
        assert!(
            endpoints.is_empty() || endpoints.iter().all(|n| !n.id.name.contains("@")),
            "parallel route slots should not produce @-prefixed ApiEndpoint nodes: {:?}",
            endpoints
        );
    }

    /// Adversarial: comment-only HTTP method names should not be detected.
    /// `// export async function GET()` is NOT an export.
    #[test]
    fn test_commented_out_exports_not_detected() {
        let content = r#"
// export async function GET(request: NextRequest) {
//   return Response.json({})
// }

/* export const POST = async () => {} */

export async function DELETE() {}
"#;
        let bindings = find_exported_http_methods(content);
        let methods: Vec<&str> = bindings.iter().map(|b| b.http_method.as_str()).collect();
        // Commented-out GET and POST must NOT be detected; only DELETE
        assert!(
            !methods.contains(&"GET"),
            "commented-out GET should not be detected, got: {:?}",
            methods
        );
        assert!(
            !methods.contains(&"POST"),
            "block-commented POST should not be detected, got: {:?}",
            methods
        );
        assert!(
            methods.contains(&"DELETE"),
            "DELETE should be detected, got: {:?}",
            methods
        );
    }

    /// Adversarial: empty roots slice must not panic.
    #[test]
    fn test_empty_roots_no_panic() {
        let result = nextjs_routing_pass(&[], &[]);
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
    }

    /// Adversarial: route file that exists but is unreadable (permissions) must not panic.
    /// We can't easily test real unreadable files in CI, so we test an empty route file
    /// (no exported methods) → should emit ANY endpoint, not panic.
    #[test]
    fn test_empty_route_file_emits_any_endpoint() {
        use std::fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();

        let route_dir = root.join("app").join("api").join("empty");
        fs::create_dir_all(&route_dir).unwrap();
        fs::write(route_dir.join("route.ts"), "").unwrap(); // empty file

        let roots = vec![("test".to_string(), root)];
        let result = nextjs_routing_pass(&roots, &[]);

        let endpoints: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert_eq!(endpoints.len(), 1, "empty file should emit ANY catch-all endpoint");
        assert_eq!(
            endpoints[0].metadata.get("http_method").map(|s| s.as_str()),
            Some("ANY")
        );
    }
}
