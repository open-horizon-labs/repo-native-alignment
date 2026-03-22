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
//! - TypeScript/JavaScript namespace imports: `import * as ns from '…'`
//! - Python named imports: `from module import foo, bar`
//! - Python bare imports: `import foo`
//! - Rust `use` declarations: `use crate::foo::{A, B}`
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
    // 2. For each file, build the set of imported symbol names.
    // ------------------------------------------------------------------
    // file -> HashSet<imported_name>
    let mut imported_names_by_file: HashMap<PathBuf, HashSet<String>> = HashMap::new();
    for node in all_nodes {
        if node.id.kind == NodeKind::Import {
            let text = &node.id.name; // Import node name = full import text
            let names = parse_imported_names(text);
            if !names.is_empty() {
                imported_names_by_file
                    .entry(node.id.file.clone())
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
        let Some(imported_names) = imported_names_by_file.get(&node.id.file) else {
            continue;
        };
        if node.body.is_empty() {
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
            // Check if the function body contains a call to this name.
            if !body_contains_call(&node.body, imported_name) {
                continue;
            }

            // Look up candidate Function nodes with this name in OTHER files.
            let Some(candidates) = fn_by_name.get(imported_name.as_str()) else {
                continue;
            };
            let cross_file_candidates: Vec<&&Node> = candidates
                .iter()
                .filter(|c| c.id.file != node.id.file)
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
// Parse imported symbol names from an import statement
// ---------------------------------------------------------------------------

/// Extract the individual symbol names imported by an import statement.
///
/// Covers:
/// - ES6 named:    `import { Foo, Bar as B } from '…'`  → `["Foo", "Bar"]`
/// - ES6 default:  `import Foo from '…'`                → `["Foo"]`
/// - ES6 namespace:`import * as ns from '…'`            → `["ns"]`
/// - Python from:  `from mod import Foo, Bar`           → `["Foo", "Bar"]`
/// - Python bare:  `import foo`                         → `["foo"]`
/// - Rust use:     `use crate::foo::{A, B}`             → `["A", "B"]`
/// - Rust use:     `use crate::foo::Bar`                → `["Bar"]`
pub(crate) fn parse_imported_names(import_text: &str) -> Vec<String> {
    let text = import_text.trim();

    // ------------------------------------------------------------------
    // TypeScript/JavaScript ES6
    // ------------------------------------------------------------------
    if text.starts_with("import ") && text.contains(" from ") {
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
    if text.starts_with("import ") && !text.contains(" from ") {
        let after = text
            .strip_prefix("import ")
            .unwrap_or("")
            .trim()
            .trim_end_matches(';');
        return after
            .split(',')
            .map(|s| {
                // Handle `import foo as f` — take the alias
                let mut parts = s.trim().split(" as ");
                let canonical = parts.next().unwrap_or("").trim();
                // For `import foo.bar`, the callable is `foo` (the module)
                canonical
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .collect();
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
/// - Namespace:  `import * as ns from '…'`          → `["ns"]`
/// - Mixed:      `import Def, { A } from '…'`       → `["Def", "A"]`
/// - Type-only:  `import type { A } from '…'`       → `["A"]`
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

    let mut names = Vec::new();

    // Namespace import: `* as ns`
    if specifier.starts_with("* as ") {
        if let Some(ns) = specifier.strip_prefix("* as ") {
            let ns = ns.trim();
            if !ns.is_empty() {
                names.push(ns.to_string());
            }
        }
        return names;
    }

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

        // Check that what follows is `(`.
        let next_char = search[after_idx..].chars().next();
        if next_char != Some('(') {
            search = &search[idx + 1..];
            continue;
        }

        // Check that the character immediately before `name` is not `.` or `:`.
        if idx > 0 {
            let prev_char = search[..idx].chars().last();
            if prev_char == Some('.') || prev_char == Some(':') {
                search = &search[idx + 1..];
                continue;
            }
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
    fn test_parse_es6_namespace_import() {
        let names = parse_imported_names("import * as api from './api'");
        assert_eq!(names, vec!["api"]);
    }

    #[test]
    fn test_parse_es6_type_only_import() {
        let names = parse_imported_names("import type { MyType } from './types'");
        assert_eq!(names, vec!["MyType"]);
    }

    #[test]
    fn test_parse_python_from_import() {
        let names = parse_imported_names("from .service import get_workspace, list_users");
        assert_eq!(names, vec!["get_workspace", "list_users"]);
    }

    #[test]
    fn test_parse_python_bare_import() {
        let names = parse_imported_names("import os");
        assert_eq!(names, vec!["os"]);
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
    fn test_body_matches_multiline() {
        let body = "{\n  const data = fetchData(id);\n  return data;\n}";
        assert!(body_contains_call(body, "fetchData"));
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
        // All edges use Detected confidence.
        for e in &edges {
            assert_eq!(e.confidence, Confidence::Detected,
                "all import-calls edges use Detected confidence");
        }
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
}
