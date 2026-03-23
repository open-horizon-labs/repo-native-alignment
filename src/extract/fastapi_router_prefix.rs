//! Post-extraction pass that resolves FastAPI `APIRouter(prefix=...)` composition
//! and `include_router(router_var, prefix=...)` calls, updating `ApiEndpoint`
//! nodes with full URL paths.
//!
//! # Problem
//!
//! RNA's route-decorator extraction captures the *local* path from each
//! decorator — the path argument of `@router.get("/expertunities")`.
//! When routers carry a prefix, the resulting `ApiEndpoint` node has path
//! `/expertunities` instead of the full `/workspaces/{id}/expertunities`.
//!
//! There are two patterns to detect:
//!
//! **Pattern 1 — `APIRouter(prefix=...)` constructor arg** (handled since PR #519):
//! ```python
//! workspace_router = APIRouter(prefix="/workspaces/{id}")
//! ```
//!
//! **Pattern 2 — `include_router(router, prefix=...)` call arg** (new in this PR):
//! ```python
//! expertunities_router = APIRouter()   # no prefix here
//! ...
//! app.include_router(expertunities_router, prefix="/workspaces/{workspace_id}/expertunities")
//! ```
//!
//! Pattern 2 is common when routers are composed in a central `main.py` or
//! `api/__init__.py` file rather than on the router definition itself.
//!
//! This causes `api_link_pass` to fail to connect TypeScript SDK URL constants
//! (which contain the full path) to FastAPI handler nodes.
//!
//! # Fix
//!
//! This pass runs **after** tree-sitter extraction and the initial
//! `api_link_pass`.
//!
//! **Phase 1 — same-file `APIRouter(prefix=...)` scan:**
//! For each Python file that contains `ApiEndpoint` nodes:
//! 1. Read the file content.
//! 2. Extract `variable = APIRouter(prefix="...")` assignments.
//! 3. For each `ApiEndpoint` node in that file whose `router_var` metadata
//!    key matches a found prefix, prepend the prefix to `http_path`.
//!
//! **Phase 2 — cross-file `include_router(router_var, prefix=...)` scan:**
//! For each workspace root:
//! 1. Walk all Python files in the root.
//! 2. Extract `include_router(var, prefix="...")` calls (any receiver).
//! 3. Merge into the same `router_var → prefix` map used in Phase 1.
//! 4. Apply to any `ApiEndpoint` nodes whose `router_var` still has no prefix.
//!
//! Phase 2 only runs when `root_pairs` is non-empty (i.e., when called from the
//! post-extraction registry with a proper `PassContext`).
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
//! - When both patterns define a prefix for the same `router_var`, the
//!   `APIRouter(prefix=...)` constructor arg takes precedence (it is the
//!   "closer to definition" binding).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::graph::{Node, NodeKind};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the FastAPI router prefix pass.
///
/// Mutates `nodes` in place: for each Python `ApiEndpoint` node whose
/// `router_var` metadata matches an `APIRouter(prefix=...)` assignment OR an
/// `include_router(router_var, prefix=...)` call found in the repository, the
/// node's `http_path` metadata, `name`, and `signature` are updated to the
/// full path.
///
/// # Arguments
///
/// * `nodes` — the full node slice (only `ApiEndpoint` Python nodes are modified)
/// * `root_pairs` — `(slug, path)` pairs for workspace roots, used to find
///   `include_router` calls in parent files.  Pass an empty slice to skip the
///   cross-file scan (e.g., in unit tests that don't need it).
///
/// # File reading
///
/// Files are read at most once per unique path (via an internal cache).
/// Non-readable files are silently skipped with a tracing warning.
pub fn fastapi_router_prefix_pass(nodes: &mut [Node], root_pairs: &[(String, PathBuf)]) {
    // --- Performance characteristics ---
    //
    // This pass is self-gating: the first thing it does is collect Python
    // ApiEndpoint nodes that have a non-empty `router_var` metadata key.  In
    // the common case — a repository with no FastAPI code, or FastAPI code that
    // uses `@app.route` directly — *none* of the nodes pass the filter, the
    // resulting set is empty, and the function returns immediately.  The cost is
    // a single linear scan of the node slice (O(n)) with no file I/O, no regex
    // compilation, and no heap allocation beyond the HashSet itself.

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
    // Result: var_name -> prefix (aggregated across all relevant files)
    let mut global_prefix_map: HashMap<String, String> = HashMap::new();

    // Phase 1: scan endpoint files for `var = APIRouter(prefix=...)` in the same file.
    // Keyed by file so we can do same-file lookups first (higher specificity).
    let mut file_prefix_map: HashMap<PathBuf, HashMap<String, String>> = HashMap::new();
    for file_path in &files_to_scan {
        match extract_prefixes_from_file(file_path) {
            Ok(prefixes) if !prefixes.is_empty() => {
                // Merge into global map (constructor-arg prefix takes precedence over include_router).
                for (var, prefix) in &prefixes {
                    global_prefix_map.entry(var.clone()).or_insert_with(|| prefix.clone());
                }
                file_prefix_map.insert(file_path.clone(), prefixes);
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

    // Phase 2: scan all Python files in the workspace roots for `include_router` calls.
    // This picks up prefixes that are set in a parent file (main.py, api/__init__.py, etc.).
    if !root_pairs.is_empty() {
        // Collect all router_var names that still need a prefix (no same-file prefix found).
        let unresolved_vars: std::collections::HashSet<String> = nodes
            .iter()
            .filter(|n| {
                n.language == "python"
                    && n.id.kind == NodeKind::ApiEndpoint
            })
            .filter_map(|n| n.metadata.get("router_var"))
            .filter(|v| !v.is_empty() && !global_prefix_map.contains_key(*v))
            .cloned()
            .collect();

        if !unresolved_vars.is_empty() {
            // Walk Python files in all roots, skip files already scanned in phase 1.
            for (_slug, root_path) in root_pairs {
                scan_python_files_for_include_router(
                    root_path,
                    &files_to_scan,
                    &unresolved_vars,
                    &mut global_prefix_map,
                );
            }
        }
    }

    if global_prefix_map.is_empty() && file_prefix_map.is_empty() {
        return;
    }

    // Step 3: update ApiEndpoint nodes whose router_var has a known prefix.
    for node in nodes.iter_mut() {
        if node.language != "python" || node.id.kind != NodeKind::ApiEndpoint {
            continue;
        }
        let router_var = match node.metadata.get("router_var") {
            Some(v) if !v.is_empty() => v.clone(),
            _ => continue,
        };

        // Prefer same-file prefix (higher specificity) over cross-file include_router prefix.
        let prefix = if let Some(file_prefixes) = file_prefix_map.get(&node.id.file) {
            file_prefixes.get(&router_var).cloned()
        } else {
            None
        }
        .or_else(|| global_prefix_map.get(&router_var).cloned());

        let prefix = match prefix {
            Some(p) if !p.is_empty() => p,
            _ => continue,
        };

        // Apply the prefix.
        apply_prefix(node, &prefix);
    }
}

// ---------------------------------------------------------------------------
// Cross-file include_router scan
// ---------------------------------------------------------------------------

/// Walk Python files under `root_path`, skipping files already in `already_scanned`,
/// and extract `include_router(var, prefix=...)` mappings for vars in `target_vars`.
///
/// Results are merged into `prefix_map`. Only vars in `target_vars` are added —
/// this avoids adding stale mappings for router vars that were already resolved
/// via the constructor-arg pattern.
fn scan_python_files_for_include_router(
    root_path: &Path,
    already_scanned: &std::collections::HashSet<PathBuf>,
    target_vars: &std::collections::HashSet<String>,
    prefix_map: &mut HashMap<String, String>,
) {
    walk_dir_for_include_router(root_path, already_scanned, target_vars, prefix_map, 0);
}

/// Recursive directory walker (depth-limited to avoid deep symlink loops).
fn walk_dir_for_include_router(
    dir: &Path,
    already_scanned: &std::collections::HashSet<PathBuf>,
    target_vars: &std::collections::HashSet<String>,
    prefix_map: &mut HashMap<String, String>,
    depth: usize,
) {
    const MAX_DEPTH: usize = 20;
    if depth > MAX_DEPTH {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // Skip hidden directories (e.g. .git, .venv, __pycache__).
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && (name.starts_with('.') || name == "__pycache__" || name == "node_modules")
        {
            continue;
        }

        if path.is_dir() {
            walk_dir_for_include_router(&path, already_scanned, target_vars, prefix_map, depth + 1);
        } else if path.extension().and_then(|e| e.to_str()) == Some("py") {
            if already_scanned.contains(&path) {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    if content.contains("include_router") {
                        let found = extract_include_router_prefixes_from_str(&content);
                        for (var, prefix) in found {
                            if target_vars.contains(&var) {
                                prefix_map.entry(var).or_insert(prefix);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("fastapi_router_prefix: could not read {:?}: {}", path, e);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract all prefix mappings from a Python source file.
///
/// Combines both `APIRouter(prefix=...)` constructor args and
/// `include_router(var, prefix=...)` call args.
///
/// Returns `Err` only if the file cannot be read.
fn extract_prefixes_from_file(
    path: &Path,
) -> Result<HashMap<String, String>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let mut map = extract_router_prefixes_from_str(&content);
    // Also pick up include_router prefixes in the same file.
    for (var, prefix) in extract_include_router_prefixes_from_str(&content) {
        map.entry(var).or_insert(prefix);
    }
    Ok(map)
}

/// Pure (testable) inner implementation for `APIRouter(prefix=...)` detection.
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

/// Pure (testable) inner implementation for `include_router(router_var, prefix=...)` detection.
///
/// Exported as `pub(crate)` so unit tests can call it without touching the
/// filesystem.
///
/// Handles patterns like:
/// ```python
/// # Direct call on app/router object (most common):
/// app.include_router(expertunities_router, prefix="/workspaces/{workspace_id}/expertunities")
/// workspace_router.include_router(comment_router, prefix="/comment")
///
/// # Keyword router argument:
/// app.include_router(router=expertunities_router, prefix="/expertunities")
///
/// # Multi-line:
/// app.include_router(
///     expertunities_router,
///     prefix="/workspaces/{workspace_id}/expertunities"
/// )
/// ```
///
/// Returns a `HashMap<var_name, prefix>` for all recognized calls.
pub(crate) fn extract_include_router_prefixes_from_str(content: &str) -> HashMap<String, String> {
    const MULTILINE_LOOKAHEAD: usize = 8;

    let mut map = HashMap::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        // Fast pre-filter: line must contain "include_router".
        if !trimmed.contains("include_router") {
            i += 1;
            continue;
        }

        // Try single-line parse first.
        if trimmed.contains("prefix") && trimmed.contains(')')
            && let Some((var, prefix)) = parse_include_router_call(trimmed)
        {
            map.insert(var, prefix);
            i += 1;
            continue;
        }

        // Multi-line: join up to MULTILINE_LOOKAHEAD continuation lines.
        let end = (i + 1 + MULTILINE_LOOKAHEAD).min(lines.len());
        let combined: String = lines[i..end]
            .iter()
            .map(|l| l.trim())
            .collect::<Vec<_>>()
            .join(" ");

        if combined.contains("prefix")
            && let Some((var, prefix)) = parse_include_router_call(&combined)
        {
            map.insert(var, prefix);
        }

        i += 1;
    }
    map
}

/// Parse a single line of the form `<expr>.include_router(<var>, prefix="...")`.
///
/// Also handles the keyword-arg form: `<expr>.include_router(router=<var>, prefix="...")`.
///
/// Returns `Some((router_var_name, prefix))` if the line matches, `None` otherwise.
fn parse_include_router_call(line: &str) -> Option<(String, String)> {
    // Must contain `.include_router(` somewhere.
    let call_start = line.find(".include_router(")?;
    let args_str = &line[call_start + ".include_router(".len()..];

    // Extract `prefix=` keyword argument.
    let prefix = extract_kwarg_string(args_str, "prefix")?;
    if !prefix.starts_with('/') {
        return None;
    }

    // Extract the router variable name — first positional arg or `router=` kwarg.
    let router_var = extract_include_router_var(args_str)?;
    if !is_valid_identifier(&router_var) {
        return None;
    }

    Some((router_var, prefix))
}

/// Extract the router variable name from `include_router` arguments string.
///
/// Handles:
/// - Positional: `(expertunities_router, prefix="...")` → `"expertunities_router"`
/// - Keyword: `(router=expertunities_router, prefix="...")` → `"expertunities_router"`
fn extract_include_router_var(args: &str) -> Option<String> {
    // Check for keyword arg `router=<ident>` first.
    if let Some(pos) = args.find("router=") {
        // Make sure this is `router=` not `include_router=` or similar.
        // The char before `router=` must be a word-boundary character.
        let before_pos = if pos > 0 { args.as_bytes()[pos - 1] } else { b'(' };
        if before_pos == b'(' || before_pos == b',' || before_pos == b' ' {
            let after = args[pos + "router=".len()..].trim_start();
            let var = take_identifier(after);
            if !var.is_empty() {
                return Some(var);
            }
        }
    }

    // Positional: first token before a `,` or `)`.
    let first_arg = args.trim_start();
    // Skip if the first arg starts with a quote (it's a string, not a variable).
    if first_arg.starts_with('"') || first_arg.starts_with('\'') {
        return None;
    }
    let var = take_identifier(first_arg);
    if !var.is_empty() {
        Some(var)
    } else {
        None
    }
}

/// Take a Python identifier from the start of `s`.
fn take_identifier(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
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
/// This function is **idempotent**: running it twice on the same node with the
/// same prefix produces the same result.  The original local path is preserved
/// in `metadata["http_path_local"]` on first application; subsequent calls
/// derive the full path from that stable value rather than the already-prefixed
/// `http_path`, preventing `/api/api/...` double-prefixing on repeated pass runs.
///
/// Updates:
/// - `metadata["http_path_local"]` — set once to the original local path
/// - `metadata["http_path"]` — full path = `prefix + local`
/// - `id.name` — `"METHOD /full/path"`
/// - `signature` — `"[route_decorator] METHOD /full/path"`
fn apply_prefix(node: &mut Node, prefix: &str) {
    // Use the already-stored local path if present (idempotency guard), otherwise
    // read the current http_path and save it as the stable local path.
    let local_path = if let Some(local) = node.metadata.get("http_path_local") {
        local.clone()
    } else {
        let raw = match node.metadata.get("http_path") {
            Some(p) => p.clone(),
            None => return,
        };
        node.metadata.insert("http_path_local".to_string(), raw.clone());
        raw
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

    #[test]
    fn test_join_paths() {
        assert_eq!(join_paths("/workspaces/{id}", "/expertunities"),
                   "/workspaces/{id}/expertunities");
        assert_eq!(join_paths("/api", "/users"), "/api/users");
        assert_eq!(join_paths("/api/", "/users"), "/api/users");
    }

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
    fn test_no_api_router_lines_returns_empty() {
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

    // ── extract_include_router_prefixes_from_str ──────────────────────────

    #[test]
    fn test_include_router_double_quoted_prefix() {
        let content = r#"app.include_router(expertunities_router, prefix="/workspaces/{workspace_id}/expertunities")"#;
        let map = extract_include_router_prefixes_from_str(content);
        assert_eq!(
            map.get("expertunities_router").map(|s| s.as_str()),
            Some("/workspaces/{workspace_id}/expertunities"),
            "include_router prefix must be extracted"
        );
    }

    #[test]
    fn test_include_router_single_quoted_prefix() {
        let content = r#"workspace_router.include_router(comment_router, prefix='/comment')"#;
        let map = extract_include_router_prefixes_from_str(content);
        assert_eq!(
            map.get("comment_router").map(|s| s.as_str()),
            Some("/comment"),
        );
    }

    #[test]
    fn test_include_router_prefix_before_router_var() {
        // prefix kwarg before the router var in args — should still parse
        // Note: this is unusual but we test robustness
        let content = r#"app.include_router(items_router, tags=["items"], prefix="/items")"#;
        let map = extract_include_router_prefixes_from_str(content);
        assert_eq!(
            map.get("items_router").map(|s| s.as_str()),
            Some("/items"),
        );
    }

    #[test]
    fn test_include_router_multiline() {
        let content = "app.include_router(\n    expertunities_router,\n    prefix=\"/workspaces/{workspace_id}/expertunities\"\n)";
        let map = extract_include_router_prefixes_from_str(content);
        assert_eq!(
            map.get("expertunities_router").map(|s| s.as_str()),
            Some("/workspaces/{workspace_id}/expertunities"),
            "multi-line include_router prefix must be extracted"
        );
    }

    #[test]
    fn test_include_router_no_prefix_ignored() {
        let content = r#"app.include_router(router_without_prefix)"#;
        let map = extract_include_router_prefixes_from_str(content);
        assert!(map.is_empty(), "include_router without prefix must produce no entry");
    }

    #[test]
    fn test_include_router_prefix_no_leading_slash_ignored() {
        let content = r#"app.include_router(bad_router, prefix="no-slash")"#;
        let map = extract_include_router_prefixes_from_str(content);
        assert!(map.is_empty(), "prefix without leading slash must be ignored");
    }

    #[test]
    fn test_multiple_include_routers_in_main() {
        let content = r#"
app.include_router(workspace_router, prefix="/workspaces")
app.include_router(user_router, prefix="/users")
app.include_router(auth_router, prefix="/auth")
"#;
        let map = extract_include_router_prefixes_from_str(content);
        assert_eq!(map.len(), 3);
        assert_eq!(map.get("workspace_router").map(|s| s.as_str()), Some("/workspaces"));
        assert_eq!(map.get("user_router").map(|s| s.as_str()), Some("/users"));
        assert_eq!(map.get("auth_router").map(|s| s.as_str()), Some("/auth"));
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

    /// Verifies that `apply_prefix` is idempotent: calling it twice with the same
    /// prefix produces the same result as calling it once (no double-prefixing).
    ///
    /// This guards against the `/api/api/...` regression that occurs when the pass
    /// is run on cached clean-root nodes during full builds or on the in-memory
    /// graph during incremental updates (CodeRabbit finding, PR #528).
    #[test]
    fn test_apply_prefix_is_idempotent() {
        let mut node = make_api_endpoint(
            "routers/workspaces.py",
            "GET /expertunities",
            "GET",
            "/expertunities",
            Some("workspace_router"),
        );
        apply_prefix(&mut node, "/workspaces/{id}");
        // Second call with the same prefix must not double-prefix.
        apply_prefix(&mut node, "/workspaces/{id}");
        assert_eq!(
            node.metadata.get("http_path").map(|s| s.as_str()),
            Some("/workspaces/{id}/expertunities"),
            "second apply_prefix call must not double-prefix"
        );
        assert_eq!(node.id.name, "GET /workspaces/{id}/expertunities");
        // The original local path must be preserved in http_path_local.
        assert_eq!(
            node.metadata.get("http_path_local").map(|s| s.as_str()),
            Some("/expertunities"),
            "http_path_local must be set to the original local path"
        );
    }

    // ── fastapi_router_prefix_pass (integration) ──────────────────────────

    /// Acceptance test: `APIRouter(prefix=...)` in same file.
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
        fastapi_router_prefix_pass(&mut nodes, &[]);

        let ep = &nodes[0];
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/workspaces/{id}/expertunities"),
            "full path must be /workspaces/{{id}}/expertunities after prefix resolution"
        );
        assert_eq!(ep.id.name, "GET /workspaces/{id}/expertunities");
    }

    /// Acceptance test (issue #517): `include_router(router_var, prefix=...)` in a parent file.
    ///
    /// Simulates the IC codebase pattern:
    /// ```python
    /// # routers/expertunities.py
    /// expertunities_router = APIRouter()  # no prefix here
    /// @expertunities_router.get("/")
    /// def list_expertunities(): ...
    ///
    /// # main.py
    /// app.include_router(expertunities_router, prefix="/workspaces/{workspace_id}/expertunities")
    /// ```
    ///
    /// After the pass, the ApiEndpoint node must have
    /// `http_path = "/workspaces/{workspace_id}/expertunities/"`.
    #[test]
    fn test_pass_include_router_cross_file_prefix() {
        use tempfile::{NamedTempFile, TempDir};
        use std::io::Write;

        // Create a temp dir representing the project root.
        let tmp_dir = TempDir::new().unwrap();

        // Write the endpoint file (no prefix on router constructor).
        let mut endpoint_file = NamedTempFile::new_in(tmp_dir.path()).unwrap();
        writeln!(endpoint_file, "expertunities_router = APIRouter()").unwrap();
        let endpoint_path = endpoint_file.path().to_path_buf();

        // Write main.py with include_router call.
        let main_py = tmp_dir.path().join("main.py");
        std::fs::write(
            &main_py,
            r#"app.include_router(expertunities_router, prefix="/workspaces/{workspace_id}/expertunities")"#,
        ).unwrap();

        let root_pairs = vec![("repo".to_string(), tmp_dir.path().to_path_buf())];
        let mut nodes = vec![
            make_api_endpoint(
                endpoint_path.to_str().unwrap(),
                "GET /",
                "GET",
                "/",
                Some("expertunities_router"),
            ),
        ];
        fastapi_router_prefix_pass(&mut nodes, &root_pairs);

        let ep = &nodes[0];
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/workspaces/{workspace_id}/expertunities/"),
            "include_router prefix must be applied to endpoint in a different file"
        );
    }

    /// When both APIRouter(prefix=...) and include_router set a prefix for the same var,
    /// the APIRouter constructor arg takes precedence.
    #[test]
    fn test_constructor_prefix_takes_precedence_over_include_router() {
        use tempfile::{NamedTempFile, TempDir};
        use std::io::Write;

        let tmp_dir = TempDir::new().unwrap();

        // Endpoint file: router has a constructor prefix.
        let mut endpoint_file = NamedTempFile::new_in(tmp_dir.path()).unwrap();
        writeln!(endpoint_file, r#"my_router = APIRouter(prefix="/constructor-prefix")"#).unwrap();
        let endpoint_path = endpoint_file.path().to_path_buf();

        // Parent file also has an include_router with a DIFFERENT prefix.
        let main_py = tmp_dir.path().join("main.py");
        std::fs::write(
            &main_py,
            r#"app.include_router(my_router, prefix="/include-router-prefix")"#,
        ).unwrap();

        let root_pairs = vec![("repo".to_string(), tmp_dir.path().to_path_buf())];
        let mut nodes = vec![
            make_api_endpoint(
                endpoint_path.to_str().unwrap(),
                "GET /endpoint",
                "GET",
                "/endpoint",
                Some("my_router"),
            ),
        ];
        fastapi_router_prefix_pass(&mut nodes, &root_pairs);

        // Constructor prefix takes precedence.
        assert_eq!(
            nodes[0].metadata.get("http_path").map(|s| s.as_str()),
            Some("/constructor-prefix/endpoint"),
            "APIRouter constructor prefix must take precedence over include_router prefix"
        );
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
        fastapi_router_prefix_pass(&mut nodes, &[]);
        assert_eq!(nodes[0].metadata.get("http_path").map(|s| s.as_str()), Some("/users"));
    }

    // ── fast path (no matching nodes → no file I/O) ───────────────────────

    #[test]
    fn test_fast_path_noop_on_empty_node_set() {
        let mut nodes: Vec<Node> = vec![];
        fastapi_router_prefix_pass(&mut nodes, &[]);
        assert!(nodes.is_empty());
    }

    #[test]
    fn test_fast_path_noop_on_no_router_var_nodes() {
        let mut nodes = vec![
            make_api_endpoint("/app/routes.py", "GET /users", "GET", "/users", None),
            make_api_endpoint("/app/routes.py", "POST /users", "POST", "/users", None),
        ];
        fastapi_router_prefix_pass(&mut nodes, &[]);
        assert_eq!(nodes[0].metadata.get("http_path").map(|s| s.as_str()), Some("/users"));
        assert_eq!(nodes[1].metadata.get("http_path").map(|s| s.as_str()), Some("/users"));
    }

    // ── multi-line APIRouter detection ────────────────────────────────────

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
        fastapi_router_prefix_pass(&mut nodes, &[]);
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
        fastapi_router_prefix_pass(&mut nodes, &[]);
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
        fastapi_router_prefix_pass(&mut nodes, &[]);

        assert_eq!(nodes[0].metadata.get("http_path").map(|s| s.as_str()), Some("/items/"));
        assert_eq!(nodes[1].metadata.get("http_path").map(|s| s.as_str()), Some("/items/"));
        assert_eq!(nodes[2].metadata.get("http_path").map(|s| s.as_str()), Some("/items/{id}"));
    }
}
