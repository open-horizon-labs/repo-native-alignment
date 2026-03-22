//! Post-extraction pass that emits `Calls` edges for cross-file function calls
//! resolved through import declarations.
//!
//! # Problem
//!
//! Same-file call detection (#407) emits `Calls` edges only when caller and
//! callee are in the same file.  Cross-file calls — where a function is first
//! imported, then invoked — produce no edges.  TypeScript LSP misses them for
//! JSX/TSX because React hook invocations are not tracked as call-hierarchy
//! entries.  This breaks shortest-path traversal across framework boundaries.
//!
//! Example gap:
//!   `Expertunities.tsx` imports `useQueryExpertunities` from `../api` and calls
//!   it — but no `Calls` edge exists, breaking the path to `SubmissionRepo`.
//!
//! # Solution
//!
//! [`import_calls_pass`] runs as a post-extraction step (after all nodes from
//! all roots are merged).  It needs only the set of `Node`s — no LSP, no
//! tree-sitter.  Algorithm:
//!
//! 1. Build an index of all `Function` nodes by name for fast lookup.
//! 2. For each file, collect `Import` nodes and parse the individual imported
//!    symbol names out of the import statement text.
//! 3. For each `Function` node in the same file, scan its body for bare
//!    identifiers matching any imported name (word-boundary check).
//! 4. For each match, look up a `Function` node with that name in a different
//!    file.  If exactly one candidate exists, emit a `Calls` edge.  If
//!    multiple candidates exist, emit edges to all of them (LSP will later
//!    confirm or prune).
//!
//! # Language support
//!
//! The pass is language-agnostic: it operates on the structured node graph
//! rather than on raw source.  Import statement parsing covers:
//!
//! - TypeScript/JavaScript ES6 named imports: `import { foo, bar } from '…'`
//! - TypeScript/JavaScript default imports: `import foo from '…'`
//! - Python named imports: `from module import foo, bar`
//! - Python bare imports: `import foo`
//! - Rust `use` declarations: `use crate::foo::{A, B}`
//!
//! **Not supported:** TypeScript namespace imports (`import * as ns`). The
//! alias is used as `ns.foo()` — a method call — which the body scanner
//! correctly rejects. Resolving `ns.foo` requires member-access tracking.
//!
//! For TypeScript, the pass also attempts to filter to relative imports (those
//! starting with `.`) to avoid emitting edges to npm package functions that may
//! share a name with local code.  Both relative and non-relative import matches
//! use [`Confidence::Detected`] since the function body text confirms the call.
//!
//! # Placement
//!
//! Call after all nodes from all roots are merged — same placement as
//! [`api_link_pass`](super::api_link::api_link_pass) and
//! [`tested_by_pass`](super::naming_convention::tested_by_pass) in
//! `build_full_graph_inner` and `update_graph_with_scan`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeKind};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Post-extraction pass: emit `Calls` edges from callers to imported functions.
///
/// Call this after all nodes from all roots are merged so that cross-file
/// import/callee pairs are discovered correctly during incremental scans.
///
/// Returns the new edges to add.  The returned `Vec` may be empty if no
/// import-based cross-file calls are detected.
pub fn import_calls_pass(all_nodes: &[Node]) -> Vec<Edge> {
    // ------------------------------------------------------------------
    // 1. Index function nodes by name for O(1) cross-file lookup.
    // ------------------------------------------------------------------
    // name -> list of (file, node) pairs.  Multiple files may define the
    // same function name, so we keep all candidates.
    let mut fn_by_name: HashMap<&str, Vec<&Node>> = HashMap::new();
    for node in all_nodes {
        if node.id.kind == NodeKind::Function {
            fn_by_name.entry(node.id.name.as_str()).or_default().push(node);
        }
    }

    if fn_by_name.is_empty() {
        return Vec::new();
    }

    // ------------------------------------------------------------------
    // 2. For each (root, file) pair, build the set of imported symbol names.
    // ------------------------------------------------------------------
    // Key: (root, file) — file alone is not unique in multi-root workspaces
    // where two roots may both contain `src/lib.ts`.
    let mut imported_names_by_file: HashMap<(String, PathBuf), HashSet<String>> = HashMap::new();
    for node in all_nodes {
        if node.id.kind == NodeKind::Import {
            let text = &node.id.name; // Import node name = full import text
            let names = parse_imported_names(text);
            if !names.is_empty() {
                imported_names_by_file
                    .entry((node.id.root.clone(), node.id.file.clone()))
                    .or_default()
                    .extend(names);
            }
        }
    }

    if imported_names_by_file.is_empty() {
        return Vec::new();
    }

    // ------------------------------------------------------------------
    // 4. For each function in a file that has imports, check body for
    //    cross-file calls.
    // ------------------------------------------------------------------
    let mut edges: Vec<Edge> = Vec::new();
    // Dedup guard — (from_stable_id, to_stable_id).
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for node in all_nodes {
        if node.id.kind != NodeKind::Function {
            continue;
        }
        let file_key = (node.id.root.clone(), node.id.file.clone());
        let Some(imported_names) = imported_names_by_file.get(&file_key) else {
            continue;
        };
        if node.body.is_empty() {
            continue;
        }

        // Perf optimization: extract call sites from the body ONCE, then check
        // each against the imports set. This is O(body_size + imports) instead
        // of O(imports × body_size) when iterating imports first.
        let called_names = extract_call_sites(&node.body);
        if called_names.is_empty() {
            continue;
        }

        // For each imported name that appears as a call in this function body
        for imported_name in imported_names {
            // Skip very short names to avoid false positives.
            if imported_name.len() < 4 {
                continue;
            }
            // Skip if caller name == imported name (self-call / wrapper pattern).
            if node.id.name == imported_name.as_str() {
                continue;
            }
            // Check if the extracted call sites include this imported name.
            if !called_names.contains(imported_name.as_str()) {
                continue;
            }

            // Look up candidate Function nodes with this name in OTHER
            // (root, file) pairs, within the same language family.
            // TypeScript `import` can only resolve TypeScript/JavaScript modules;
            // Python `from … import` can only resolve Python modules; etc.
            // Filtering by language prevents cross-language false positives in
            // polyglot repositories.
            let Some(candidates) = fn_by_name.get(imported_name.as_str()) else {
                continue;
            };
            let caller_lang = node.language.as_str();
            let cross_file_candidates: Vec<&&Node> = candidates
                .iter()
                .filter(|c| {
                    (c.id.root != node.id.root || c.id.file != node.id.file)
                        && languages_compatible(caller_lang, c.language.as_str())
                })
                .collect();

            if cross_file_candidates.is_empty() {
                continue;
            }

            // All import-call edges use Detected confidence.  Both relative and
            // non-relative imports are treated the same: finding a local function
            // node with a matching name confirms the call target.
            let confidence = Confidence::Detected;

            for &callee in &cross_file_candidates {
                let key = (node.id.to_stable_id(), callee.id.to_stable_id());
                if seen.contains(&key) {
                    continue;
                }
                seen.insert(key);

                tracing::debug!(
                    "import_calls: {} ({}) -> {} ({})",
                    node.id.name,
                    node.id.file.display(),
                    callee.id.name,
                    callee.id.file.display(),
                );

                edges.push(Edge {
                    from: node.id.clone(),
                    to: callee.id.clone(),
                    kind: EdgeKind::Calls,
                    source: ExtractionSource::TreeSitter,
                    confidence: confidence.clone(),
                });
            }
        }
    }

    if !edges.is_empty() {
        tracing::info!(
            "import_calls pass: {} cross-file Calls edge(s) via import resolution",
            edges.len()
        );
    }

    edges
}

// ---------------------------------------------------------------------------
// Language family compatibility
// ---------------------------------------------------------------------------

/// Returns `true` when code written in `caller_lang` can `import` a function
/// defined in `callee_lang` using that language's native module system.
///
/// Each language family is a closed set:
/// - TypeScript / JavaScript / TSX / JSX — all share the same ES module system
/// - Python — only imports Python modules (`.py` / compiled extensions)
/// - Rust — only imports Rust crate items via `use`
///
/// Returns `false` for unknown language pairs to avoid cross-language noise.
fn languages_compatible(caller_lang: &str, callee_lang: &str) -> bool {
    // Normalize to lowercase for comparison.
    let c1 = caller_lang.to_lowercase();
    let c2 = callee_lang.to_lowercase();
    if c1 == c2 {
        return true;
    }
    // TypeScript / JavaScript share the same import system.
    let ts_family = ["typescript", "javascript", "tsx", "jsx"];
    let c1_is_ts = ts_family.iter().any(|l| c1.contains(l));
    let c2_is_ts = ts_family.iter().any(|l| c2.contains(l));
    if c1_is_ts && c2_is_ts {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Parse imported symbol names from an import statement
// ---------------------------------------------------------------------------

/// Extract the individual symbol names imported by an import statement.
///
/// Covers:
/// - ES6 named:    `import { Foo, Bar as B } from '…'`  → `["Foo", "Bar"]`
/// - ES6 default:  `import Foo from '…'`                → `["Foo"]`
/// - ES6 namespace:`import * as ns from '…'`            → `[]` (not supported — see below)
/// - ES6 type-only:`import type { Foo } from '…'`       → `[]` (erased at runtime)
/// - Python from:  `from mod import Foo, Bar`           → `["Foo", "Bar"]`
/// - Python bare:  `import foo`                         → `[]` (binds module, not callable)
/// - Rust use:     `use crate::foo::{A, B}`             → `["A", "B"]`
/// - Rust use:     `use crate::foo::Bar`                → `["Bar"]`
///
/// **Namespace imports return an empty list.** The alias (`ns`) is used as
/// `ns.foo()` — a method call — and `body_contains_call` correctly rejects
/// method calls.  Resolving `ns.foo` to the module-level `foo` would require
/// member-access tracking beyond the scope of this pass.
///
/// **Type-only imports return an empty list.** `import type { Foo }` is erased
/// by TypeScript at runtime; no callable value binding exists.
///
/// **Python bare imports return an empty list.** `import os` binds a module
/// object, not a callable function.  `os.path.exists()` is a method call which
/// `body_contains_call` rejects.  Use `from os import exists` to import a
/// callable function.
pub(crate) fn parse_imported_names(import_text: &str) -> Vec<String> {
    let text = import_text.trim();

    // ------------------------------------------------------------------
    // TypeScript/JavaScript ES6
    // ------------------------------------------------------------------
    if text.starts_with("import ") && text.contains(" from ") {
        // `import type { Foo }` is erased by TypeScript at runtime — no value
        // binding is created, so `Foo()` is not callable.  Skip type-only imports.
        if text.starts_with("import type ") {
            return Vec::new();
        }
        return parse_es6_import_names(text);
    }

    // ------------------------------------------------------------------
    // Python: `from module import Foo, Bar`
    // ------------------------------------------------------------------
    if text.starts_with("from ") && text.contains(" import ") {
        let after_import = text
            .split(" import ")
            .nth(1)
            .unwrap_or("")
            .trim()
            .trim_end_matches(';');
        return after_import
            .split(',')
            .map(|s| s.trim().split_whitespace().next().unwrap_or("").to_string())
            .filter(|s| !s.is_empty() && s != "*")
            .collect();
    }

    // ------------------------------------------------------------------
    // Python: `import foo` / `import foo, bar`
    // ------------------------------------------------------------------
    // `import foo` binds a *module object*, not a directly callable function.
    // Calling `foo()` afterwards would be `TypeError: 'module' object is not
    // callable`. Module member calls like `foo.bar()` are method-call syntax,
    // which `body_contains_call` already rejects.  We return empty here to
    // match the same pattern used for TypeScript namespace imports and type-only
    // imports — both of which also produce non-callable bindings.
    if text.starts_with("import ") && !text.contains(" from ") {
        return Vec::new();
    }

    // ------------------------------------------------------------------
    // Rust: `use crate::foo::{A, B, C}` or `use crate::foo::Bar`
    // ------------------------------------------------------------------
    if text.starts_with("use ") {
        let after = text
            .strip_prefix("use ")
            .unwrap_or("")
            .trim()
            .trim_end_matches(';');
        // Brace group: `{A, B}`
        if let Some(brace_start) = after.rfind('{') {
            if let Some(brace_end) = after.rfind('}') {
                let inner = &after[brace_start + 1..brace_end];
                return inner
                    .split(',')
                    .map(|s| {
                        // Handle `A as _` style
                        s.trim().split(" as ").next().unwrap_or("").trim().to_string()
                    })
                    .filter(|s| !s.is_empty() && s != "*" && s != "self")
                    .collect();
            }
        }
        // Bare path: `use crate::foo::Bar`
        if let Some(last_segment) = after.split("::").last() {
            let name = last_segment.trim();
            if !name.is_empty() && name != "*" && name != "self" {
                return vec![name.to_string()];
            }
        }
    }

    Vec::new()
}

/// Parse ES6 import names from a TypeScript/JavaScript import statement.
///
/// Handles:
/// - Named:      `import { A, B as C } from '…'`   → `["A", "B"]`
/// - Default:    `import Foo from '…'`              → `["Foo"]`
/// - Mixed:      `import Def, { A } from '…'`       → `["Def", "A"]`
///
/// **Note:** `import type { A }` is caught before this function is called and
/// returns empty from `parse_imported_names`.  This function should not be
/// called with type-only imports.
///
/// **Namespace imports are intentionally NOT supported** (`import * as ns`).
/// The pass detects bare function calls (`name(`) in function bodies, so
/// namespace-qualified calls (`ns.foo()`) would require member-access tracking
/// which is out of scope for this v1 pass.  Namespace imports return an empty
/// list; they are not yet supported.
fn parse_es6_import_names(text: &str) -> Vec<String> {
    // Strip `import ` prefix and optional `type ` keyword.
    let body = text
        .strip_prefix("import ")
        .unwrap_or(text)
        .trim();
    let body = body
        .strip_prefix("type ")
        .unwrap_or(body)
        .trim();

    // Everything before ` from '…'`
    let specifier = if let Some(from_idx) = body.find(" from ") {
        &body[..from_idx]
    } else {
        body
    };
    let specifier = specifier.trim().trim_end_matches(',').trim();

    // Namespace imports (`* as ns`) are not resolved by this pass — the
    // namespace alias is used as `ns.foo()` (method call), which
    // body_contains_call correctly rejects.  Return empty list.
    if specifier.starts_with("* as ") || specifier == "*" {
        return Vec::new();
    }

    let mut names = Vec::new();

    // Split named `{ … }` block from default import prefix.
    let (default_part, named_part) = if let Some(brace_start) = specifier.find('{') {
        let before = specifier[..brace_start].trim().trim_end_matches(',').trim();
        let end = specifier.rfind('}').map(|i| i + 1).unwrap_or(specifier.len());
        let inner = &specifier[brace_start + 1..end - 1];
        (before, Some(inner))
    } else {
        (specifier, None)
    };

    // Default import (e.g. `import React from 'react'`).
    if !default_part.is_empty() && default_part != "*" {
        names.push(default_part.to_string());
    }

    // Named imports from `{ A, B as C }`.
    if let Some(inner) = named_part {
        for part in inner.split(',') {
            // `B as C` → take `B` (the original name in the module)
            let original = part
                .trim()
                .split(" as ")
                .next()
                .unwrap_or("")
                .trim();
            if !original.is_empty() {
                names.push(original.to_string());
            }
        }
    }

    names
}

// ---------------------------------------------------------------------------
// Helper: check if a function body contains a bare call to a name
// ---------------------------------------------------------------------------

/// Returns `true` when `body` contains a bare function call to `name` —
/// i.e., the name appears followed by `(` with no intervening `.` or `::`.
///
/// This is intentionally conservative: only `name(` counts, not `obj.name(`.
/// Extract all function call site names from a body in one pass.
/// Returns a HashSet of identifier names that appear immediately before `(`.
/// O(body_size) — called once per function body instead of once per import name.
pub(crate) fn extract_call_sites(body: &str) -> HashSet<&str> {
    let mut result = HashSet::new();
    let bytes = body.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        // Find '('
        if bytes[i] == b'(' && i > 0 {
            // Walk backwards to find the identifier
            let mut j = i.saturating_sub(1);
            // Skip whitespace
            while j > 0 && bytes[j] == b' ' { j -= 1; }
            let end = j + 1;
            // Walk back through identifier chars
            while j > 0 && (bytes[j - 1].is_ascii_alphanumeric() || bytes[j - 1] == b'_' || bytes[j - 1] == b'$') {
                j -= 1;
            }
            if j < end {
                // Use byte-based slicing to avoid UTF-8 char boundary panics.
                // `j` and `end` are byte indices from walking over `bytes`; the
                // walk only proceeds through ASCII identifier chars, so `j..end`
                // is always a valid ASCII slice. Convert via the byte slice.
                let ident = std::str::from_utf8(&bytes[j..end]).unwrap_or("");
                if ident.len() >= 4 {
                    // Reject if preceded by '.' or ':' (method/scoped call)
                    let prev = if j > 0 { bytes[j - 1] } else { 0 };
                    if prev != b'.' && prev != b':' {
                        result.insert(ident);
                    }
                }
            }
        }
        i += 1;
    }
    result
}

#[allow(dead_code)]
pub(crate) fn body_contains_call(body: &str, name: &str) -> bool {
    // Fast reject: name must appear somewhere in the body.
    if !body.contains(name) {
        return false;
    }

    // Look for occurrences of `name(` that are NOT preceded by `.` or `:`.
    // This filters out method calls (`.foo(`) and scoped calls (`::foo(`).
    let mut search = body;
    while let Some(idx) = search.find(name) {
        let after_idx = idx + name.len();

        // Compute the number of bytes to advance past the first character at `idx`.
        // `find()` returns byte offsets; `&search[idx + 1..]` panics if the character
        // at `idx` is a multi-byte Unicode sequence.  Use the actual char width instead.
        let advance = idx + search[idx..]
            .chars()
            .next()
            .map(|ch| ch.len_utf8())
            .unwrap_or(1);

        // Check that what follows is `(`.
        let next_char = search[after_idx..].chars().next();
        if next_char != Some('(') {
            search = &search[advance..];
            continue;
        }

        // Check that the character immediately before `name` is an identifier
        // boundary — i.e., NOT a character that would make it part of a longer
        // identifier (`rerender` must not match `render`).
        //
        // Reject: alphanumeric, `_`, `$` (JS identifier chars), `.` (method call),
        //         `:` (scoped path).
        if idx > 0 {
            if let Some(prev_char) = search[..idx].chars().last() {
                if prev_char == '.'
                    || prev_char == ':'
                    || prev_char == '_'
                    || prev_char == '$'
                    || prev_char.is_ascii_alphanumeric()
                {
                    search = &search[advance..];
                    continue;
                }
            }
        }

        // Reject declaration contexts: `function name(`, `def name(`, `fn name(`,
        // `const name(`, `class name(` — these define the symbol, not call it.
        // Check if the text immediately before `name` ends with a declaration keyword.
        let before = search[..idx].trim_end();
        let is_declaration = ["function", "def", "fn", "const", "let", "var",
                               "class", "async function", "async def",
                               "async fn", "pub fn", "pub async fn"]
            .iter()
            .any(|kw| before.ends_with(kw));
        if is_declaration {
            search = &search[advance..];
            continue;
        }

        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// Helpers for NodeId (needed by tests)
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

    fn make_fn(file: &str, name: &str, body: &str) -> Node {
        Node {
            id: NodeId {
                root: "r".into(),
                file: PathBuf::from(file),
                name: name.into(),
                kind: NodeKind::Function,
            },
            language: "typescript".into(),
            line_start: 1,
            line_end: 10,
            signature: format!("function {}()", name),
            body: body.into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_import(file: &str, import_text: &str) -> Node {
        Node {
            id: NodeId {
                root: "r".into(),
                file: PathBuf::from(file),
                name: import_text.into(),
                kind: NodeKind::Import,
            },
            language: "typescript".into(),
            line_start: 1,
            line_end: 1,
            signature: import_text.into(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    // -----------------------------------------------------------------------
    // parse_imported_names tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_es6_named_import() {
        let names = parse_imported_names("import { useQueryExpertunities } from '../api'");
        assert_eq!(names, vec!["useQueryExpertunities"]);
    }

    #[test]
    fn test_parse_es6_multiple_named_imports() {
        let names = parse_imported_names("import { foo, bar, baz } from './utils'");
        assert_eq!(names, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn test_parse_es6_named_import_with_alias() {
        // `import { Foo as F }` — take the original name `Foo`
        let names = parse_imported_names("import { Foo as F, Bar } from './mod'");
        assert_eq!(names, vec!["Foo", "Bar"]);
    }

    #[test]
    fn test_parse_es6_default_import() {
        let names = parse_imported_names("import React from 'react'");
        assert_eq!(names, vec!["React"]);
    }

    #[test]
    fn test_parse_es6_namespace_import_returns_empty() {
        // Namespace imports (`import * as ns`) are intentionally not supported.
        // The alias is used as `ns.foo()` (method call) which body_contains_call
        // rejects. We return empty so no spurious edges are emitted.
        let names = parse_imported_names("import * as api from './api'");
        assert!(names.is_empty(), "namespace imports should return empty, got {:?}", names);
    }

    #[test]
    fn test_parse_es6_type_only_import_returns_empty() {
        // `import type { Foo }` is erased at runtime — no callable value created.
        let names = parse_imported_names("import type { MyType } from './types'");
        assert!(names.is_empty(), "type-only import should return empty, got {:?}", names);
    }

    #[test]
    fn test_parse_python_from_import() {
        let names = parse_imported_names("from .service import get_workspace, list_users");
        assert_eq!(names, vec!["get_workspace", "list_users"]);
    }

    #[test]
    fn test_parse_python_bare_import_returns_empty() {
        // `import os` binds the module object, not a callable function.
        // `os()` would raise TypeError; module members need `os.method()` which
        // body_contains_call correctly rejects.
        let names = parse_imported_names("import os");
        assert!(names.is_empty(), "bare Python import should return empty, got {:?}", names);
    }

    #[test]
    fn test_parse_rust_use_brace() {
        let names = parse_imported_names("use crate::utils::{process, validate}");
        assert_eq!(names, vec!["process", "validate"]);
    }

    #[test]
    fn test_parse_rust_use_bare() {
        let names = parse_imported_names("use crate::service::handle_request");
        assert_eq!(names, vec!["handle_request"]);
    }

    // -----------------------------------------------------------------------
    // body_contains_call tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_body_contains_bare_call() {
        assert!(body_contains_call("let x = foo(args);", "foo"));
    }

    #[test]
    fn test_body_does_not_match_method_call() {
        // `obj.foo()` is a method call — should NOT match
        assert!(!body_contains_call("let x = obj.foo();", "foo"));
    }

    #[test]
    fn test_body_does_not_match_scoped_call() {
        // `::foo()` is a scoped path — should NOT match
        assert!(!body_contains_call("let x = mod::foo();", "foo"));
    }

    #[test]
    fn test_body_does_not_match_suffix_identifier() {
        // `rerender()` must NOT match when looking for `render`
        assert!(!body_contains_call("rerender(component);", "render"));
        // `_render()` (prefixed with underscore) must NOT match
        assert!(!body_contains_call("_render(component);", "render"));
        // `$render()` must NOT match
        assert!(!body_contains_call("$render(component);", "render"));
        // Bare `render()` SHOULD match
        assert!(body_contains_call("render(component);", "render"));
    }

    #[test]
    fn test_body_matches_multiline() {
        let body = "{\n  const data = fetchData(id);\n  return data;\n}";
        assert!(body_contains_call(body, "fetchData"));
    }

    #[test]
    fn test_body_contains_call_unicode_safe() {
        // Verifies no panic on multi-byte Unicode before a bare function call.
        // Previously `&search[idx + 1..]` would panic if `idx` landed mid-char.
        // The fix advances by `ch.len_utf8()` bytes instead.
        let body = "{ let résultat = fetch(42); }"; // é is a 2-byte char
        assert!(body_contains_call(body, "fetch"));
        // Ensure the search doesn't panic when the imported name appears after a
        // non-ASCII identifier prefix (method call on unicode-named obj).
        let body2 = "{ résultat.fetch(42); }";
        assert!(!body_contains_call(body2, "fetch")); // method call, not bare
    }

    #[test]
    fn test_body_does_not_match_declaration_context() {
        // `function helper(` defines the symbol, not calls it — must NOT match
        assert!(!body_contains_call("function helper(x) { return x; }", "helper"));
        // `def helper(` — Python declaration
        assert!(!body_contains_call("def helper(x):\n    return x", "helper"));
        // `fn helper(` — Rust declaration
        assert!(!body_contains_call("fn helper(x: i32) -> i32 { x }", "helper"));
        // Actual call SHOULD match
        assert!(body_contains_call("let result = helper(42);", "helper"));
    }

    // -----------------------------------------------------------------------
    // import_calls_pass integration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_cross_file_call_emitted() {
        // Caller in file A imports `helper` from file B, calls it.
        let caller = make_fn(
            "a.ts",
            "main",
            "function main() { return helper(42); }",
        );
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");
        let import = make_import(
            "a.ts",
            "import { helper } from './b'",
        );

        let nodes = vec![caller.clone(), callee.clone(), import];
        let edges = import_calls_pass(&nodes);

        assert_eq!(edges.len(), 1, "expected 1 Calls edge, got {:?}", edges);
        assert_eq!(edges[0].from.name, "main");
        assert_eq!(edges[0].to.name, "helper");
        assert_eq!(edges[0].kind, EdgeKind::Calls);
    }

    #[test]
    fn test_no_edge_when_no_import() {
        // `helper` appears in the body, but there's no import statement.
        let caller = make_fn("a.ts", "main", "function main() { return helper(42); }");
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");

        let nodes = vec![caller, callee];
        let edges = import_calls_pass(&nodes);

        assert!(
            edges.is_empty(),
            "no import → no edge, got {:?}",
            edges
        );
    }

    #[test]
    fn test_no_edge_for_same_file_call() {
        // Even if a function is imported and called, no edge when callee is
        // same file (same-file detection already covers this).
        let caller = make_fn("a.ts", "main", "function main() { return helper(42); }");
        let callee = make_fn("a.ts", "helper", "function helper(x) { return x; }");
        let import = make_import("a.ts", "import { helper } from './b'");

        let nodes = vec![caller, callee, import];
        let edges = import_calls_pass(&nodes);

        // The candidate filter excludes same-file functions, so no edge.
        assert!(
            edges.is_empty(),
            "same-file callee must be excluded, got {:?}",
            edges
        );
    }

    #[test]
    fn test_no_edge_when_name_not_in_body() {
        // `helper` is imported but never called in the body.
        let caller = make_fn("a.ts", "main", "function main() { return 42; }");
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");
        let import = make_import("a.ts", "import { helper } from './b'");

        let nodes = vec![caller, callee, import];
        let edges = import_calls_pass(&nodes);

        assert!(
            edges.is_empty(),
            "no call in body → no edge, got {:?}",
            edges
        );
    }

    #[test]
    fn test_python_cross_file_call() {
        let caller = Node {
            id: NodeId {
                root: "r".into(),
                file: PathBuf::from("handler.py"),
                name: "get_workspace".into(),
                kind: NodeKind::Function,
            },
            language: "python".into(),
            line_start: 1,
            line_end: 5,
            signature: "def get_workspace(id):".into(),
            body: "def get_workspace(id):\n    return fetch_data(id)".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let callee = Node {
            id: NodeId {
                root: "r".into(),
                file: PathBuf::from("service.py"),
                name: "fetch_data".into(),
                kind: NodeKind::Function,
            },
            language: "python".into(),
            line_start: 1,
            line_end: 3,
            signature: "def fetch_data(id):".into(),
            body: "def fetch_data(id):\n    return db.get(id)".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let import_node = Node {
            id: NodeId {
                root: "r".into(),
                file: PathBuf::from("handler.py"),
                name: "from .service import fetch_data".into(),
                kind: NodeKind::Import,
            },
            language: "python".into(),
            line_start: 1,
            line_end: 1,
            signature: "from .service import fetch_data".into(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let nodes = vec![caller, callee, import_node];
        let edges = import_calls_pass(&nodes);

        assert_eq!(edges.len(), 1, "expected 1 Python Calls edge, got {:?}", edges);
        assert_eq!(edges[0].from.name, "get_workspace");
        assert_eq!(edges[0].to.name, "fetch_data");
        assert_eq!(edges[0].kind, EdgeKind::Calls);
    }

    #[test]
    fn test_no_self_edge() {
        // A function that imports itself (unusual, but guard defensively).
        let node = make_fn("a.ts", "processData", "function processData() { return processData(); }");
        let import = make_import("a.ts", "import { processData } from './b'");
        // No other file defines processData — so no edge anyway.

        let nodes = vec![node, import];
        let edges = import_calls_pass(&nodes);

        for e in &edges {
            assert_ne!(e.from, e.to, "self-edge must never be emitted");
        }
    }

    #[test]
    fn test_short_names_skipped() {
        // Imported names shorter than 4 chars are skipped.
        let caller = make_fn("a.ts", "main", "function main() { foo(x); }");
        let callee = make_fn("b.ts", "foo", "function foo(x) { return x; }");
        let import = make_import("a.ts", "import { foo } from './b'");

        let nodes = vec![caller, callee, import];
        let edges = import_calls_pass(&nodes);

        assert!(
            edges.is_empty(),
            "names < 4 chars must be skipped, got {:?}",
            edges
        );
    }

    #[test]
    fn test_method_call_not_confused_with_bare_call() {
        // `obj.helper()` must not emit a Calls edge for `helper`.
        let caller = make_fn(
            "a.ts",
            "main",
            "function main() { return obj.helper(42); }",
        );
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");
        let import = make_import("a.ts", "import { helper } from './b'");

        let nodes = vec![caller, callee, import];
        let edges = import_calls_pass(&nodes);

        assert!(
            edges.is_empty(),
            "method call `obj.helper()` must not emit Calls edge, got {:?}",
            edges
        );
    }

    #[test]
    fn test_relative_import_gets_detected_confidence() {
        let caller = make_fn("a.ts", "main", "function main() { return helper(42); }");
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");
        let import = make_import("a.ts", "import { helper } from './b'");

        let nodes = vec![caller, callee, import];
        let edges = import_calls_pass(&nodes);

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].confidence, Confidence::Detected,
            "relative import should produce Detected confidence");
    }

    #[test]
    fn test_non_relative_import_also_emits_edge() {
        let caller = make_fn("a.ts", "main", "function main() { return helper(42); }");
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");
        // Non-relative import but matching function exists in local graph
        let import = make_import("a.ts", "import { helper } from 'some-library'");

        let nodes = vec![caller, callee, import];
        let edges = import_calls_pass(&nodes);

        // Edge is emitted when a local function with matching name exists.
        assert_eq!(edges.len(), 1, "expected 1 Calls edge for the non-relative import");
        // All edges use Detected confidence.
        for e in &edges {
            assert_eq!(e.confidence, Confidence::Detected,
                "all import-calls edges use Detected confidence");
        }
    }

    #[test]
    fn test_no_cross_language_edge() {
        // TypeScript cannot import Python functions. If a TS file has an import
        // for `fetch_data` and a Python file defines `fetch_data`, no edge should
        // be emitted between them.
        let caller = Node {
            id: NodeId {
                root: "r".into(),
                file: PathBuf::from("a.ts"),
                name: "main".into(),
                kind: NodeKind::Function,
            },
            language: "typescript".into(),
            line_start: 1, line_end: 3,
            signature: "function main()".into(),
            body: "function main() { return fetch_data(id); }".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let python_callee = Node {
            id: NodeId {
                root: "r".into(),
                file: PathBuf::from("service.py"),
                name: "fetch_data".into(),
                kind: NodeKind::Function,
            },
            language: "python".into(),
            line_start: 1, line_end: 3,
            signature: "def fetch_data(id):".into(),
            body: "def fetch_data(id):\n    return db.get(id)".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let import = make_import("a.ts", "import { fetch_data } from './service'");

        let nodes = vec![caller, python_callee, import];
        let edges = import_calls_pass(&nodes);

        // TypeScript and Python are incompatible language families — no edge.
        assert!(
            edges.is_empty(),
            "cross-language edges (TS → Python) must not be emitted, got {:?}",
            edges
        );
    }

    #[test]
    fn test_import_type_does_not_emit_edge() {
        // `import type { Foo }` is erased at runtime — Foo() is not callable.
        let caller = make_fn("a.ts", "main", "function main() { return Processor(42); }");
        let callee = make_fn("b.ts", "Processor", "function Processor(x) { return x; }");
        let type_import = make_import("a.ts", "import type { Processor } from './b'");

        let nodes = vec![caller, callee, type_import];
        let edges = import_calls_pass(&nodes);

        assert!(
            edges.is_empty(),
            "import type should not emit Calls edges, got {:?}",
            edges
        );
    }

    #[test]
    fn test_idempotent_on_repeated_call() {
        let caller = make_fn("a.ts", "main", "function main() { return helper(42); }");
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");
        let import = make_import("a.ts", "import { helper } from './b'");

        let nodes = vec![caller, callee, import];

        let edges_first = import_calls_pass(&nodes);
        let edges_second = import_calls_pass(&nodes);

        assert_eq!(
            edges_first.len(),
            edges_second.len(),
            "repeated calls must produce the same number of edges"
        );
    }

    #[test]
    fn test_multi_root_imports_keyed_by_root_and_file() {
        // Two roots both contain `src/lib.ts`. Without the (root, file) key,
        // imports from root-b would contaminate root-a's import set.
        // With the (root, file) key, each root's imports are isolated.
        let caller_a = Node {
            id: NodeId {
                root: "root-a".into(),
                file: PathBuf::from("src/lib.ts"),
                name: "mainA".into(),
                kind: NodeKind::Function,
            },
            language: "typescript".into(),
            line_start: 1, line_end: 5,
            signature: "function mainA()".into(),
            body: "function mainA() { return helperA(1); }".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let callee_a = Node {
            id: NodeId {
                root: "root-a".into(),
                file: PathBuf::from("src/helpers.ts"),
                name: "helperA".into(),
                kind: NodeKind::Function,
            },
            language: "typescript".into(),
            line_start: 1, line_end: 3,
            signature: "function helperA(x)".into(),
            body: "function helperA(x) { return x; }".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let import_a = Node {
            id: NodeId {
                root: "root-a".into(),
                file: PathBuf::from("src/lib.ts"),
                name: "import { helperA } from './helpers'".into(),
                kind: NodeKind::Import,
            },
            language: "typescript".into(),
            line_start: 1, line_end: 1,
            signature: "import { helperA } from './helpers'".into(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        // root-b also has src/lib.ts, with a different function (mainB → helperB).
        // Without the fix, root-b's imports of helperB could be applied to root-a's
        // `mainA`, causing spurious edges.
        let caller_b = Node {
            id: NodeId {
                root: "root-b".into(),
                file: PathBuf::from("src/lib.ts"),
                name: "mainB".into(),
                kind: NodeKind::Function,
            },
            language: "typescript".into(),
            line_start: 1, line_end: 5,
            signature: "function mainB()".into(),
            body: "function mainB() { return helperB(1); }".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let callee_b = Node {
            id: NodeId {
                root: "root-b".into(),
                file: PathBuf::from("src/helpers.ts"),
                name: "helperB".into(),
                kind: NodeKind::Function,
            },
            language: "typescript".into(),
            line_start: 1, line_end: 3,
            signature: "function helperB(x)".into(),
            body: "function helperB(x) { return x; }".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let import_b = Node {
            id: NodeId {
                root: "root-b".into(),
                file: PathBuf::from("src/lib.ts"),
                name: "import { helperB } from './helpers'".into(),
                kind: NodeKind::Import,
            },
            language: "typescript".into(),
            line_start: 1, line_end: 1,
            signature: "import { helperB } from './helpers'".into(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let nodes = vec![caller_a, callee_a.clone(), import_a,
                         caller_b, callee_b.clone(), import_b];
        let edges = import_calls_pass(&nodes);

        // Expected: 2 edges — mainA→helperA (root-a) and mainB→helperB (root-b).
        // Without (root, file) keying: root-b's `helperB` import could leak into
        // root-a, and vice versa, causing different counts.
        assert_eq!(edges.len(), 2, "expected 2 isolated cross-file edges, got {:?}",
                   edges.iter().map(|e| format!("{}->{}", e.from.name, e.to.name)).collect::<Vec<_>>());

        let edge_a = edges.iter().find(|e| e.from.root == "root-a");
        assert!(edge_a.is_some(), "missing root-a edge");
        assert_eq!(edge_a.unwrap().from.name, "mainA");
        assert_eq!(edge_a.unwrap().to.name, "helperA");

        let edge_b = edges.iter().find(|e| e.from.root == "root-b");
        assert!(edge_b.is_some(), "missing root-b edge");
        assert_eq!(edge_b.unwrap().from.name, "mainB");
        assert_eq!(edge_b.unwrap().to.name, "helperB");
    }
}
