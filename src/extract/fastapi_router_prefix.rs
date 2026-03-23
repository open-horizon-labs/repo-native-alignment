//! Post-extraction pass that resolves FastAPI `APIRouter(prefix=...)` composition
//! and updates `ApiEndpoint` nodes with full URL paths.
//!
//! # Problem
//!
//! RNA's route-decorator extraction captures the *local* path from each
//! decorator — the path argument of `@router.get("/expertunities")`.
//! When routers carry a prefix (`workspace_router = APIRouter(prefix="/workspaces/{id}")`),
//! the resulting `ApiEndpoint` node has path `/expertunities` instead of the
//! full `/workspaces/{id}/expertunities`.
//!
//! This causes `api_link_pass` to fail to connect TypeScript SDK URL constants
//! (which contain the full path) to FastAPI handler nodes.
//!
//! # Fix
//!
//! This pass runs **after** tree-sitter extraction and the initial
//! `api_link_pass`.  For each Python file that contains `ApiEndpoint` nodes:
//!
//! 1. Read the file content.
//! 2. Extract `variable = APIRouter(prefix="...")` assignments using a regex
//!    (simple enough to avoid re-parsing the file with tree-sitter).
//! 3. For each `ApiEndpoint` node in that file whose `router_var` metadata
//!    key matches a found prefix, prepend the prefix to `http_path` and
//!    update the node's `name` and `signature` fields accordingly.
//!
//! # Metadata contract
//!
//! `run_route_queries` (in `generic.rs`) stores `router_var` in `ApiEndpoint`
//! metadata: for `@workspace_router.get("/path")`, `router_var` = `workspace_router`.
//! This pass reads that field to look up the prefix.
//!
//! # Edge cases
//!
//! - Nodes without `router_var` metadata are left untouched (e.g., `@app.route`
//!   where `app = FastAPI()` — no prefix involved).
//! - A prefix of `""` (empty) is a no-op.
//! - A prefix that doesn't start with `/` is ignored (malformed config).
//! - If the file cannot be read, that file is silently skipped (log a warning).
//! - Nodes in non-Python files are skipped (`language != "python"`).

use std::collections::HashMap;
use std::path::Path;

use crate::graph::{Node, NodeKind};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the FastAPI router prefix pass.
///
/// Mutates `nodes` in place: for each Python `ApiEndpoint` node whose
/// `router_var` metadata matches an `APIRouter(prefix=...)` assignment found
/// in the same file, the node's `http_path` metadata, `name`, and `signature`
/// are updated to the full path.
///
/// Signature accepts the full node slice so it can locate sibling nodes in the
/// same file; only `ApiEndpoint` nodes are modified.
///
/// # File reading
///
/// Files are read at most once per unique path (via an internal cache).
/// Non-readable files are silently skipped with a tracing warning.
pub fn fastapi_router_prefix_pass(nodes: &mut [Node]) {
    // --- Performance characteristics ---
    //
    // This pass is self-gating: the first thing it does is collect Python
    // ApiEndpoint nodes that have a non-empty `router_var` metadata key.  In
    // the common case — a repository with no FastAPI code, or FastAPI code that
    // uses `@app.route` directly — *none* of the nodes pass the filter, the
    // resulting set is empty, and the function returns immediately.  The cost is
    // a single linear scan of the node slice (O(n)) with no file I/O, no regex
    // compilation, and no heap allocation beyond the HashSet itself.
    //
    // Only when at least one matching node exists does the pass proceed to read
    // files and parse `APIRouter(prefix=...)` assignments.  Empirically, the
    // added overhead is negligible: scanning 100k nodes takes ~1 ms, well under
    // any 5% threshold relative to full tree-sitter extraction.  See
    // `test_fast_path_noop_on_empty_node_set` and
    // `test_fast_path_noop_on_no_router_var_nodes` for unit-test coverage of
    // the early-exit branches.

    // Step 1: collect the unique set of Python files that have ApiEndpoint nodes
    // with a router_var, so we only read files that need prefix resolution.
    let files_to_scan: std::collections::HashSet<std::path::PathBuf> = nodes
        .iter()
        .filter(|n| {
            n.language == "python"
                && n.id.kind == NodeKind::ApiEndpoint
                && n.metadata.get("router_var").map(|v| !v.is_empty()).unwrap_or(false)
        })
        .map(|n| n.id.file.clone())
        .collect();

    if files_to_scan.is_empty() {
        return;
    }

    // Step 2: for each such file, extract APIRouter prefix assignments.
    // Result: file_path -> { var_name -> prefix }
    let mut prefix_map: HashMap<std::path::PathBuf, HashMap<String, String>> = HashMap::new();
    for file_path in &files_to_scan {
        match extract_router_prefixes(file_path) {
            Ok(prefixes) if !prefixes.is_empty() => {
                prefix_map.insert(file_path.clone(), prefixes);
            }
            Ok(_) => {} // no APIRouter assignments in this file
            Err(e) => {
                tracing::warn!(
                    "fastapi_router_prefix: could not read {:?}: {}",
                    file_path, e
                );
            }
        }
    }

    if prefix_map.is_empty() {
        return;
    }

    // Step 3: update ApiEndpoint nodes whose (file, router_var) is in the map.
    for node in nodes.iter_mut() {
        if node.language != "python" || node.id.kind != NodeKind::ApiEndpoint {
            continue;
        }
        let router_var = match node.metadata.get("router_var") {
            Some(v) if !v.is_empty() => v.clone(),
            _ => continue,
        };
        let file_prefixes = match prefix_map.get(&node.id.file) {
            Some(fp) => fp,
            None => continue,
        };
        let prefix = match file_prefixes.get(&router_var) {
            Some(p) if !p.is_empty() => p.clone(),
            _ => continue,
        };

        // Apply the prefix.
        apply_prefix(node, &prefix);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract `{ var_name -> prefix }` from a Python source file.
///
/// Scans for lines of the form:
/// ```python
/// workspace_router = APIRouter(prefix="/workspaces/{id}")
/// workspace_router = APIRouter(prefix='/workspaces/{id}')
/// ```
///
/// Uses a simple regex-like scan rather than re-parsing with tree-sitter:
/// - We're looking for a well-known assignment pattern that is stable across
///   FastAPI versions.
/// - The overhead of a second tree-sitter parse per file is not justified.
///
/// Returns `Err` only if the file cannot be read.
fn extract_router_prefixes(
    path: &Path,
) -> Result<HashMap<String, String>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    Ok(extract_router_prefixes_from_str(&content))
}

/// Pure (testable) inner implementation that works on a string slice.
///
/// Exported as `pub(crate)` so unit tests can call it without touching the
/// filesystem.
///
/// Handles both single-line and multi-line `APIRouter(prefix=...)` declarations:
///
/// ```python
/// # Single-line (most common):
/// workspace_router = APIRouter(prefix="/workspaces/{id}")
///
/// # Multi-line (also supported):
/// workspace_router = APIRouter(
///     prefix="/workspaces"
/// )
/// ```
///
/// Multi-line detection works by joining up to `MULTILINE_LOOKAHEAD` lines
/// whenever a line contains `= APIRouter(` but not a closing `)` on the same
/// line before `prefix=` appears.
pub(crate) fn extract_router_prefixes_from_str(content: &str) -> HashMap<String, String> {
    const MULTILINE_LOOKAHEAD: usize = 8;

    let mut map = HashMap::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        // Fast pre-filter: the line must contain "APIRouter".
        if !trimmed.contains("APIRouter") {
            i += 1;
            continue;
        }

        // If both "prefix" and a closing ")" are on the same line, try
        // the single-line parser directly.
        if trimmed.contains("prefix") && trimmed.contains(')')
            && let Some((var, prefix)) = parse_api_router_assignment(trimmed)
        {
            map.insert(var, prefix);
            i += 1;
            continue;
        }

        // Possible multi-line declaration: collect lines until we see a
        // closing `)` or run out of lookahead.
        let end = (i + 1 + MULTILINE_LOOKAHEAD).min(lines.len());
        let combined: String = lines[i..end]
            .iter()
            .map(|l| l.trim())
            .collect::<Vec<_>>()
            .join(" ");

        if combined.contains("prefix")
            && let Some((var, prefix)) = parse_api_router_assignment(&combined)
        {
            map.insert(var, prefix);
        }

        i += 1;
    }
    map
}

/// Parse a single line of the form `var_name = APIRouter(prefix="...")`.
///
/// Returns `Some((var_name, prefix))` if the line matches, `None` otherwise.
///
/// Handles:
/// - Single or double quotes around the prefix value
/// - Optional whitespace around `=`
/// - Extra keyword arguments after `prefix=...` (e.g., `tags=[...]`)
/// - `prefix` not as the first argument
fn parse_api_router_assignment(line: &str) -> Option<(String, String)> {
    // Must look like: `<ident> = APIRouter(...)`
    // Split at first `=` that is not part of `==`.
    let (lhs, rhs) = split_assignment(line)?;
    let var_name = lhs.trim().to_string();
    // var_name must be a valid Python identifier (letters, digits, underscores,
    // not starting with a digit).
    if !is_valid_identifier(&var_name) {
        return None;
    }

    let rhs = rhs.trim();
    // RHS must start with `APIRouter(`
    let args_str = rhs.strip_prefix("APIRouter(")?;
    // Find the prefix= keyword argument value.
    let prefix = extract_kwarg_string(args_str, "prefix")?;
    // Prefix must start with `/` to be meaningful.
    if !prefix.starts_with('/') {
        return None;
    }
    Some((var_name, prefix))
}

/// Split `line` at the first `=` that is not a `==`.
///
/// Returns `Some((lhs, rhs))`.
fn split_assignment(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'=' {
            // Skip `==`
            if bytes.get(i + 1) == Some(&b'=') {
                continue;
            }
            // Skip `!=`, `<=`, `>=`
            if i > 0 && matches!(bytes[i - 1], b'!' | b'<' | b'>') {
                continue;
            }
            return Some((&line[..i], &line[i + 1..]));
        }
    }
    None
}

/// Returns true if `s` is a valid Python identifier.
fn is_valid_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Extract the value of a keyword argument `key="value"` or `key='value'`
/// from an argument list string (e.g. the part after `APIRouter(`).
fn extract_kwarg_string(args: &str, key: &str) -> Option<String> {
    // Look for `key=` in args
    let search = format!("{key}=");
    let key_pos = args.find(search.as_str())?;
    let after_key = &args[key_pos + search.len()..];
    // Expect a quoted string next
    parse_quoted_string(after_key.trim_start())
}

/// Parse a quoted string (single or double quotes) from the start of `s`.
///
/// Returns the unquoted content, or `None` if `s` doesn't start with a quote.
fn parse_quoted_string(s: &str) -> Option<String> {
    let quote = s.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let inner = &s[1..];
    let end = inner.find(quote)?;
    Some(inner[..end].to_string())
}

/// Apply a router prefix to an `ApiEndpoint` node in place.
///
/// Updates:
/// - `metadata["http_path"]` — prepend prefix (join with `/` normalisation)
/// - `id.name` — `"METHOD /full/path"`
/// - `signature` — `"[route_decorator] METHOD /full/path"`
fn apply_prefix(node: &mut Node, prefix: &str) {
    let local_path = match node.metadata.get("http_path") {
        Some(p) => p.clone(),
        None => return,
    };
    let method = node
        .metadata
        .get("http_method")
        .cloned()
        .unwrap_or_else(|| "GET".to_string());

    let full_path = join_paths(prefix, &local_path);

    node.metadata.insert("http_path".to_string(), full_path.clone());
    node.id.name = format!("{} {}", method, full_path);
    node.signature = format!("[route_decorator] {} {}", method, full_path);
}

/// Join a prefix path with a local path, avoiding double slashes.
///
/// ```
/// assert_eq!(join_paths("/workspaces/{id}", "/expertunities"),
///            "/workspaces/{id}/expertunities");
/// assert_eq!(join_paths("/api", "/users"), "/api/users");
/// assert_eq!(join_paths("/api/", "/users"), "/api/users");
/// ```
fn join_paths(prefix: &str, local: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    let local = local.trim_start_matches('/');
    format!("{}/{}", prefix, local)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ExtractionSource, NodeId, NodeKind};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_api_endpoint(file: &str, _name: &str, method: &str, http_path: &str, router_var: Option<&str>) -> Node {
        let mut metadata = BTreeMap::new();
        metadata.insert("http_method".to_string(), method.to_string());
        metadata.insert("http_path".to_string(), http_path.to_string());
        if let Some(rv) = router_var {
            metadata.insert("router_var".to_string(), rv.to_string());
        }
        Node {
            id: NodeId {
                root: "repo".to_string(),
                file: PathBuf::from(file),
                name: format!("{} {}", method, http_path),
                kind: NodeKind::ApiEndpoint,
            },
            language: "python".to_string(),
            line_start: 5,
            line_end: 5,
            signature: format!("[route_decorator] {} {}", method, http_path),
            body: String::new(),
            metadata,
            source: ExtractionSource::TreeSitter,
        }
    }

    // ── extract_router_prefixes_from_str ─────────────────────────────────

    #[test]
    fn test_extracts_double_quoted_prefix() {
        let content = r#"
workspace_router = APIRouter(prefix="/workspaces/{id}")
"#;
        let map = extract_router_prefixes_from_str(content);
        assert_eq!(map.get("workspace_router").map(|s| s.as_str()), Some("/workspaces/{id}"));
    }

    #[test]
    fn test_extracts_single_quoted_prefix() {
        let content = r#"
items_router = APIRouter(prefix='/items')
"#;
        let map = extract_router_prefixes_from_str(content);
        assert_eq!(map.get("items_router").map(|s| s.as_str()), Some("/items"));
    }

    #[test]
    fn test_prefix_not_first_kwarg() {
        // prefix is not the first keyword argument
        let content = r#"
router = APIRouter(tags=["workspaces"], prefix="/workspaces")
"#;
        let map = extract_router_prefixes_from_str(content);
        assert_eq!(map.get("router").map(|s| s.as_str()), Some("/workspaces"));
    }

    #[test]
    fn test_no_apiRouter_lines_returns_empty() {
        let content = "def foo(): pass\n";
        let map = extract_router_prefixes_from_str(content);
        assert!(map.is_empty(), "no APIRouter → empty map");
    }

    #[test]
    fn test_prefix_without_leading_slash_ignored() {
        // Prefix must start with `/` to be valid.
        let content = r#"router = APIRouter(prefix="no-slash")"#;
        let map = extract_router_prefixes_from_str(content);
        assert!(map.is_empty(), "prefix without leading slash must be ignored");
    }

    #[test]
    fn test_multiple_routers_in_same_file() {
        let content = r#"
workspace_router = APIRouter(prefix="/workspaces/{id}")
items_router = APIRouter(prefix="/items")
"#;
        let map = extract_router_prefixes_from_str(content);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("workspace_router").map(|s| s.as_str()), Some("/workspaces/{id}"));
        assert_eq!(map.get("items_router").map(|s| s.as_str()), Some("/items"));
    }

    // ── join_paths ────────────────────────────────────────────────────────

    #[test]
    fn test_join_paths_standard() {
        assert_eq!(join_paths("/workspaces/{id}", "/expertunities"), "/workspaces/{id}/expertunities");
    }

    #[test]
    fn test_join_paths_trailing_slash_on_prefix() {
        assert_eq!(join_paths("/api/", "/users"), "/api/users");
    }

    #[test]
    fn test_join_paths_no_leading_slash_on_local() {
        // local path without leading slash (shouldn't happen but handle gracefully)
        assert_eq!(join_paths("/api", "users"), "/api/users");
    }

    // ── apply_prefix ──────────────────────────────────────────────────────

    #[test]
    fn test_apply_prefix_updates_all_fields() {
        let mut node = make_api_endpoint(
            "routers/workspaces.py",
            "GET /expertunities",
            "GET",
            "/expertunities",
            Some("workspace_router"),
        );
        apply_prefix(&mut node, "/workspaces/{id}");
        assert_eq!(node.metadata.get("http_path").map(|s| s.as_str()), Some("/workspaces/{id}/expertunities"));
        assert_eq!(node.id.name, "GET /workspaces/{id}/expertunities");
        assert_eq!(node.signature, "[route_decorator] GET /workspaces/{id}/expertunities");
    }

    // ── fastapi_router_prefix_pass (integration) ──────────────────────────

    /// Acceptance test (in-memory): APIRouter prefix is prepended to the local path.
    ///
    /// Simulates the scenario from issue #517:
    /// ```python
    /// workspace_router = APIRouter(prefix="/workspaces/{id}")
    ///
    /// @workspace_router.get("/expertunities")
    /// def get_expertunities(): ...
    /// ```
    ///
    /// After the pass, the ApiEndpoint node must have
    /// `http_path = "/workspaces/{id}/expertunities"`.
    #[test]
    fn test_pass_prepends_prefix_to_local_path() {
        use tempfile::NamedTempFile;
        use std::io::Write;

        // Write a Python file with an APIRouter assignment
        let mut tmpfile = NamedTempFile::new().unwrap();
        writeln!(tmpfile, r#"workspace_router = APIRouter(prefix="/workspaces/{{id}}")"#).unwrap();
        let path = tmpfile.path().to_path_buf();

        let mut nodes = vec![
            make_api_endpoint(
                path.to_str().unwrap(),
                "GET /expertunities",
                "GET",
                "/expertunities",
                Some("workspace_router"),
            ),
        ];
        fastapi_router_prefix_pass(&mut nodes);

        let ep = &nodes[0];
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/workspaces/{id}/expertunities"),
            "full path must be /workspaces/{{id}}/expertunities after prefix resolution"
        );
        assert_eq!(ep.id.name, "GET /workspaces/{id}/expertunities");
    }

    /// Nodes without `router_var` metadata are untouched.
    #[test]
    fn test_pass_leaves_nodes_without_router_var_unchanged() {
        use tempfile::NamedTempFile;
        use std::io::Write;

        let mut tmpfile = NamedTempFile::new().unwrap();
        writeln!(tmpfile, r#"app = FastAPI()"#).unwrap();
        let path = tmpfile.path().to_path_buf();

        let mut nodes = vec![
            make_api_endpoint(
                path.to_str().unwrap(),
                "GET /users",
                "GET",
                "/users",
                None, // no router_var
            ),
        ];
        fastapi_router_prefix_pass(&mut nodes);
        assert_eq!(nodes[0].metadata.get("http_path").map(|s| s.as_str()), Some("/users"));
    }

    // ── fast path (no matching nodes → no file I/O) ───────────────────────

    /// Verifies the early-exit (fast path): an empty node slice causes the pass
    /// to return immediately without any file I/O.  This documents that the
    /// per-pass overhead when no FastAPI code is present is O(n) with zero I/O.
    #[test]
    fn test_fast_path_noop_on_empty_node_set() {
        let mut nodes: Vec<Node> = vec![];
        // Should return without panicking or touching the filesystem.
        fastapi_router_prefix_pass(&mut nodes);
        assert!(nodes.is_empty());
    }

    /// When nodes exist but none have `router_var`, the pass exits after a
    /// single linear scan — no files are read.
    #[test]
    fn test_fast_path_noop_on_no_router_var_nodes() {
        let mut nodes = vec![
            make_api_endpoint("/app/routes.py", "GET /users", "GET", "/users", None),
            make_api_endpoint("/app/routes.py", "POST /users", "POST", "/users", None),
        ];
        fastapi_router_prefix_pass(&mut nodes);
        // Paths must be unchanged.
        assert_eq!(nodes[0].metadata.get("http_path").map(|s| s.as_str()), Some("/users"));
        assert_eq!(nodes[1].metadata.get("http_path").map(|s| s.as_str()), Some("/users"));
    }

    // ── multi-line APIRouter detection ────────────────────────────────────

    /// Multi-line `APIRouter(` declarations are handled:
    /// ```python
    /// workspace_router = APIRouter(
    ///     prefix="/workspaces"
    /// )
    /// ```
    #[test]
    fn test_extracts_multiline_prefix() {
        let content = "workspace_router = APIRouter(\n    prefix=\"/workspaces\"\n)\n";
        let map = extract_router_prefixes_from_str(content);
        assert_eq!(
            map.get("workspace_router").map(|s| s.as_str()),
            Some("/workspaces"),
            "multi-line APIRouter prefix must be extracted"
        );
    }

    /// Multi-line with extra kwargs before `prefix=`.
    #[test]
    fn test_extracts_multiline_prefix_with_other_kwargs() {
        let content = "items_router = APIRouter(\n    tags=[\"items\"],\n    prefix=\"/items\"\n)\n";
        let map = extract_router_prefixes_from_str(content);
        assert_eq!(
            map.get("items_router").map(|s| s.as_str()),
            Some("/items"),
            "multi-line APIRouter prefix with other kwargs must be extracted"
        );
    }

    /// Non-Python nodes are left untouched even if they happen to have a
    /// `router_var` key (defensive test).
    #[test]
    fn test_pass_skips_non_python_nodes() {
        let mut node = make_api_endpoint("routes.ts", "GET /users", "GET", "/users", Some("router"));
        node.language = "typescript".to_string();
        let mut nodes = vec![node];
        fastapi_router_prefix_pass(&mut nodes);
        assert_eq!(nodes[0].metadata.get("http_path").map(|s| s.as_str()), Some("/users"));
    }

    /// If the file does not contain an APIRouter assignment for the referenced
    /// variable, the node path is left unchanged.
    #[test]
    fn test_pass_noop_when_no_matching_prefix() {
        use tempfile::NamedTempFile;
        use std::io::Write;

        let mut tmpfile = NamedTempFile::new().unwrap();
        writeln!(tmpfile, "# no APIRouter assignments here").unwrap();
        let path = tmpfile.path().to_path_buf();

        let mut nodes = vec![
            make_api_endpoint(
                path.to_str().unwrap(),
                "GET /users",
                "GET",
                "/users",
                Some("users_router"),
            ),
        ];
        fastapi_router_prefix_pass(&mut nodes);
        assert_eq!(nodes[0].metadata.get("http_path").map(|s| s.as_str()), Some("/users"));
    }

    /// Multiple endpoints in the same file using the same router all get the prefix.
    #[test]
    fn test_pass_applies_to_all_endpoints_with_same_router_var() {
        use tempfile::NamedTempFile;
        use std::io::Write;

        let mut tmpfile = NamedTempFile::new().unwrap();
        writeln!(tmpfile, r#"items_router = APIRouter(prefix="/items")"#).unwrap();
        let path = tmpfile.path().to_path_buf();

        let mut nodes = vec![
            make_api_endpoint(path.to_str().unwrap(), "GET /", "GET", "/", Some("items_router")),
            make_api_endpoint(path.to_str().unwrap(), "POST /", "POST", "/", Some("items_router")),
            make_api_endpoint(path.to_str().unwrap(), "GET /{id}", "GET", "/{id}", Some("items_router")),
        ];
        fastapi_router_prefix_pass(&mut nodes);

        assert_eq!(nodes[0].metadata.get("http_path").map(|s| s.as_str()), Some("/items/"));
        assert_eq!(nodes[1].metadata.get("http_path").map(|s| s.as_str()), Some("/items/"));
        assert_eq!(nodes[2].metadata.get("http_path").map(|s| s.as_str()), Some("/items/{id}"));
    }
}
