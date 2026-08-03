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
    // 1. Index function nodes by canonical and lexical name for O(1)
    //    cross-file lookup. Scoped function identities remain authoritative;
    //    lexical aliases exist only to bind parser-owned call metadata.
    // ------------------------------------------------------------------
    // name -> list of (file, node) pairs.  Multiple files may define the
    // same function name, so we keep all candidates.
    let mut fn_by_name: HashMap<&str, Vec<&Node>> = HashMap::new();
    for node in all_nodes {
        if node.id.kind == NodeKind::Function {
            fn_by_name
                .entry(node.id.name.as_str())
                .or_default()
                .push(node);
            if let Some(lexical_name) = node.metadata.get("lexical_name")
                && lexical_name != &node.id.name
            {
                fn_by_name
                    .entry(lexical_name.as_str())
                    .or_default()
                    .push(node);
            }
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


        // For each imported name that appears as a call in this function body
        for imported_name in imported_names {
            // Skip names known to be common builtins / accessors. The previous
            // blanket `len() < 4` filter dropped legitimate short imports
            // (`init`, `tick`, `save`); the stopword list keeps the noisy
            // names out without the false negatives.
            if is_call_stopword(imported_name.as_str()) {
                continue;
            }
            // Skip if caller name == imported name (self-call / wrapper pattern).
            if node.id.name == imported_name.as_str() {
                continue;
            }
            // Check if the imported name appears as a call site (`name(`).
            // We intentionally do NOT look for bare-name occurrences in body
            // text here: a byte-level whole-word match would also fire inside
            // comments, string literals, or shadowing declarations, which would
            // incorrectly keep callees alive for dead-code analysis.
            let is_called = called_names.contains(imported_name.as_str());
            if !is_called {
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
                    evidence: Vec::new(),
                });
            }
        }
    }

    let calls_count = edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .count();
    // ------------------------------------------------------------------
    // 5. Emit ReferencedBy edges for imports that resolve to functions.
    //    This catches references that aren't bare call sites (keyword args,
    //    dict dispatch, decorator registration, re-exports). A function
    //    imported by another file is referenced even if not called directly.
    // ------------------------------------------------------------------
    let mut ref_count = 0usize;
    for import_node in all_nodes {
        if import_node.id.kind != NodeKind::Import {
            continue;
        }
        let text = &import_node.id.name;
        let names = parse_imported_names(text);
        let importer_lang = import_node.language.as_str();
        let module_path = parse_import_module(text);
        for name in &names {
            if is_call_stopword(name.as_str()) {
                continue;
            }
            let Some(candidates) = fn_by_name.get(name.as_str()) else {
                continue;
            };

            // Only emit a ReferencedBy edge when the import's target is
            // unambiguous. Name-only resolution over an entire monorepo links
            // e.g. `import { init }` to every `init` function, which falsely
            // keeps unrelated callees alive. Either the name has exactly one
            // compatible candidate, or the module path parsed from the import
            // statement uniquely identifies the callee file.
            let compatible: Vec<&&Node> = candidates
                .iter()
                .filter(|c| {
                    (c.id.root != import_node.id.root
                        || c.id.file != import_node.id.file)
                        && languages_compatible(importer_lang, c.language.as_str())
                })
                .collect();
            if compatible.is_empty() {
                continue;
            }
            let resolved: Vec<&&Node> = if let Some(ref module) = module_path {
                // An explicit module path was written; require a compatible
                // candidate whose file matches that path before accepting it.
                // Falling back to `candidates.len() == 1` here would wrongly
                // accept a local `init` for `import { init } from 'some-library'`.
                let narrowed: Vec<&&Node> = compatible
                    .iter()
                    .copied()
                    .filter(|c| import_path_matches_file(module, &c.id.file))
                    .collect();
                if narrowed.len() == 1 { narrowed } else { continue; }
            } else if compatible.len() == 1 {
                compatible
            } else {
                continue;
            };
            for callee in resolved {
                let key = (import_node.id.to_stable_id(), callee.id.to_stable_id());
                if seen.contains(&key) {
                    continue;
                }
                seen.insert(key);
                edges.push(Edge {
                    from: import_node.id.clone(),
                    to: callee.id.clone(),
                    kind: EdgeKind::ReferencedBy,
                    source: ExtractionSource::TreeSitter,
                    confidence: Confidence::Detected,
                    evidence: Vec::new(),
                });
                ref_count += 1;
            }
        }
    }

    // ------------------------------------------------------------------
    // 6. Emit ReferencedBy edges from attribute access in function bodies.
    //    When a function body contains `obj.method_name()`, the attr_refs
    //    metadata (extracted at parse time by GenericExtractor) includes
    //    "method_name". Match its lexical alias against fn_by_name while
    //    retaining the canonical owner-qualified target identity. Multiple
    //    eligible owners are ambiguous and fail closed.
    // ------------------------------------------------------------------
    let mut attr_ref_count = 0usize;
    for node in all_nodes {
        if node.id.kind != NodeKind::Function {
            continue;
        }
        let Some(attr_refs_str) = node.metadata.get("attr_refs") else {
            continue;
        };
        let caller_lang = node.language.as_str();
        for attr_name in attr_refs_str.split(',') {
            let attr_name = attr_name.trim();
            if attr_name.is_empty() || is_call_stopword(attr_name) {
                continue;
            }
            let Some(candidates) = fn_by_name.get(attr_name) else {
                continue;
            };
            // Restrict to cross-file, same-language-family, top-level functions.
            // Nested/local function defs carry `parent_scope_kind = function` in
            // metadata and would otherwise fan out `obj.helper()` to every
            // same-named inner helper in the codebase.
            let eligible: Vec<&&Node> = candidates
                .iter()
                .filter(|c| {
                    if callee_is_nested_local(c) {
                        return false;
                    }
                    if c.id.root == node.id.root && c.id.file == node.id.file {
                        return false;
                    }
                    languages_compatible(caller_lang, c.language.as_str())
                })
                .collect();
            // Only emit when the attribute name resolves unambiguously. Any
            // fan-out at this stage turns common method names (`save`,
            // `render`) into large false-live clusters during dead-code runs.
            if eligible.len() != 1 {
                continue;
            }
            let callee = eligible[0];
            let key = (node.id.to_stable_id(), callee.id.to_stable_id());
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            edges.push(Edge {
                from: node.id.clone(),
                to: callee.id.clone(),
                kind: EdgeKind::ReferencedBy,
                source: ExtractionSource::TreeSitter,
                confidence: Confidence::Detected,
                evidence: Vec::new(),
            });
            attr_ref_count += 1;
        }
    }

    if calls_count > 0 || ref_count > 0 || attr_ref_count > 0 {
        tracing::info!(
            "import_calls pass: {} Calls, {} import ReferencedBy, {} attribute ReferencedBy edge(s)",
            calls_count,
            ref_count,
            attr_ref_count
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
    // Fast path: same language (case-insensitive, no allocation).
    if caller_lang.eq_ignore_ascii_case(callee_lang) {
        return true;
    }
    // TypeScript / JavaScript share the same import system.
    const TS_FAMILY: [&str; 4] = ["typescript", "javascript", "tsx", "jsx"];
    fn in_ts_family(lang: &str) -> bool {
        // Case-insensitive substring match without allocation.
        let lang_bytes = lang.as_bytes();
        TS_FAMILY.iter().any(|needle| {
            let n = needle.as_bytes();
            if lang_bytes.len() < n.len() {
                return false;
            }
            lang_bytes
                .windows(n.len())
                .any(|w| w.eq_ignore_ascii_case(n))
        })
    }
    in_ts_family(caller_lang) && in_ts_family(callee_lang)
}

/// Returns `true` when `callee` is a function defined inside another function
/// or closure, as recorded by the generic extractor in
/// `metadata["parent_scope_kind"]`. Such nested/local helpers are not valid
/// cross-file `ReferencedBy` targets for a bare `obj.method_name()` attribute
/// access in another file.
fn callee_is_nested_local(callee: &Node) -> bool {
    callee.metadata.get("parent_scope_kind").map(|s| s.as_str()) == Some("function")
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
            .map(|s| s.split_whitespace().next().unwrap_or("").to_string())
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
        if let Some(brace_start) = after.rfind('{')
            && let Some(brace_end) = after.rfind('}')
        {
            let inner = &after[brace_start + 1..brace_end];
            return inner
                .split(',')
                .map(|s| {
                    // Handle `A as _` style
                    s.trim()
                        .split(" as ")
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string()
                })
                .filter(|s| !s.is_empty() && s != "*" && s != "self")
                .collect();
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

/// Extract the module/path string from an import statement.
///
/// Returns a path-like string representing the source module when parseable,
/// without quotes. Returns `None` for bare `import foo` or `import foo, bar`
/// where there is no source spec (those bind module objects, not specific names).
fn parse_import_module(text: &str) -> Option<String> {
    let trimmed = text.trim();
    // ES6: `import ... from '…'` or `import ... from "…"`
    if let Some(idx) = trimmed.find(" from ") {
        let after = trimmed[idx + " from ".len()..].trim();
        let stripped = after
            .trim_end_matches(';')
            .trim()
            .trim_matches(|c: char| c == '\'' || c == '"' || c == '`');
        if !stripped.is_empty() {
            return Some(stripped.to_string());
        }
    }
    // Python: `from module.path import ...`
    if let Some(rest) = trimmed.strip_prefix("from ")
        && let Some(idx) = rest.find(" import ")
    {
        let module = rest[..idx].trim();
        if !module.is_empty() {
            return Some(module.to_string());
        }
    }
    // Rust: `use crate::foo::Bar` or `use crate::foo::{A, B}`
    if let Some(rest) = trimmed.strip_prefix("use ") {
        let stripped = rest.trim().trim_end_matches(';').trim();
        // Drop a trailing brace group for path purposes.
        let module = if let Some(brace_idx) = stripped.find('{') {
            stripped[..brace_idx].trim_end_matches("::").trim()
        } else if let Some(last_sep) = stripped.rfind("::") {
            stripped[..last_sep].trim()
        } else {
            stripped
        };
        if !module.is_empty() {
            return Some(module.to_string());
        }
    }
    None
}

/// Best-effort check that a module path from an import statement likely refers
/// to the given callee file.
///
/// Splits the module on common separators (`/`, `.`, `::`) into non-trivial
/// segments and requires the last informative segment to equal the file stem
/// or basename. When the last segment instead matches an ancestor *directory*
/// name, only accept the file when it is an index-like re-export module
/// (`index.*`, `__init__.py`, `mod.rs`). Leading `.` / `crate` / `self` /
/// `super` segments are ignored so `./b` and `../api` still work.
fn import_path_matches_file(module: &str, file: &std::path::Path) -> bool {
    let segments: Vec<&str> = module
        .split(['/', '.', ':', '\\'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "crate" && *s != "self" && *s != "super")
        .collect();
    let Some(last) = segments.last() else {
        return false;
    };
    let file_stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let file_name = file
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if *last == file_stem || *last == file_name {
        return true;
    }
    // Folder imports (e.g. `../api`, `from pkg import ...`): only accept when
    // the callee lives in an index-like re-export module inside that directory.
    // Matching any descendant file here would let `../api` resolve to an
    // arbitrary `api/*.ts` file even when the import clearly points at `api/index.ts`.
    let is_index_module = matches!(file_name, "index.ts" | "index.tsx" | "index.js" | "index.jsx" | "index.mjs" | "index.cjs" | "__init__.py" | "mod.rs");
    if !is_index_module {
        return false;
    }
    file.ancestors().any(|a| {
        a.file_name()
            .and_then(|s| s.to_str())
            .map(|n| n == *last)
            .unwrap_or(false)
    })
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
    let body = text.strip_prefix("import ").unwrap_or(text).trim();
    let body = body.strip_prefix("type ").unwrap_or(body).trim();

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
        let Some(brace_end) = specifier[brace_start + 1..].find('}').map(|i| brace_start + 1 + i) else {
            return Vec::new();
        };
        let inner = &specifier[brace_start + 1..brace_end];
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
            let original = part.trim().split(" as ").next().unwrap_or("").trim();
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

/// Extract all bare function call site names from a body in one pass.
/// Returns a HashSet of identifier names that appear immediately before `(`.
/// O(body_size) — called once per function body instead of once per import name.
///
/// Skips occurrences inside string literals (`'`, `"`, backtick) and comments
/// (`//`, `/* */`, `#`) so that `"call(name)"` in a docstring or
/// `// call(name)` in a code comment do not falsely keep `name` alive for
/// dead-code analysis. The lexer is intentionally permissive: it covers the
/// common forms across the languages this pass runs over (TS, JS, Python,
/// Rust, Go) without growing into a full per-language tokenizer. Edge cases
/// like Python triple-quoted strings, Rust raw strings (`r#"…"#`), and
/// nested template-literal expressions (``` `${call()}` ```) are still
/// considered "in string" by the simple-quote rules; the worst case is
/// dropping a real call inside a template expression, which only loses a
/// possible edge — never a false positive.
pub(crate) fn extract_call_sites(body: &str) -> HashSet<&str> {
    let mut result = HashSet::new();
    let bytes = body.as_bytes();
    let len = bytes.len();

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Code,
        LineComment,        // // … or # … to end of line
        BlockComment,       // /* … */
        StringDouble,       // "…"
        StringSingle,       // '…'
        StringBacktick,     // `…`
    }
    let mut mode = Mode::Code;
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        match mode {
            Mode::LineComment => {
                if b == b'\n' {
                    mode = Mode::Code;
                }
                i += 1;
                continue;
            }
            Mode::BlockComment => {
                if b == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                    mode = Mode::Code;
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            Mode::StringDouble | Mode::StringSingle | Mode::StringBacktick => {
                if b == b'\\' && i + 1 < len {
                    // skip the escaped byte (covers \" \' \\ \n etc.)
                    i += 2;
                    continue;
                }
                let closer = match mode {
                    Mode::StringDouble => b'"',
                    Mode::StringSingle => b'\'',
                    Mode::StringBacktick => b'`',
                    _ => unreachable!(),
                };
                if b == closer {
                    mode = Mode::Code;
                }
                i += 1;
                continue;
            }
            Mode::Code => {}
        }

        // Enter comment / string modes.
        if b == b'/' && i + 1 < len {
            match bytes[i + 1] {
                b'/' => {
                    mode = Mode::LineComment;
                    i += 2;
                    continue;
                }
                b'*' => {
                    mode = Mode::BlockComment;
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        if b == b'#' {
            // Python / Ruby / Bash / TOML line comment. Skip in TS/JS/Rust
            // a `#` is rare outside attributes (`#[…]`) and string interpolation;
            // those patterns don't precede a `(` we'd misclassify as a call,
            // so the conservative behaviour is fine.
            mode = Mode::LineComment;
            i += 1;
            continue;
        }
        if b == b'"' {
            mode = Mode::StringDouble;
            i += 1;
            continue;
        }
        if b == b'\'' {
            mode = Mode::StringSingle;
            i += 1;
            continue;
        }
        if b == b'`' {
            mode = Mode::StringBacktick;
            i += 1;
            continue;
        }

        if b == b'(' && i > 0 {
            // Walk backwards to find the identifier.
            let mut j = i.saturating_sub(1);
            while j > 0 && bytes[j] == b' ' {
                j -= 1;
            }
            let end = j + 1;
            while j > 0
                && (bytes[j - 1].is_ascii_alphanumeric()
                    || bytes[j - 1] == b'_'
                    || bytes[j - 1] == b'$')
            {
                j -= 1;
            }
            if j < end {
                // Byte slice is ASCII because the walk only accepted ASCII
                // identifier bytes; conversion is infallible in practice.
                let ident = std::str::from_utf8(&bytes[j..end]).unwrap_or("");
                if !ident.is_empty() && !is_call_stopword(ident) {
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

/// Common short identifiers that almost always denote builtins, accessors,
/// or trivial verbs rather than the kind of cross-file callable a
/// `ReferencedBy` edge should reach. Keeping these out of the call-site set
/// stops `obj.id()`-style noise from drowning out real edges in the index.
///
/// This list replaces the old blanket `len() < 4` filter so legitimate short
/// names (`init`, `tick`, `save`, `read`, `data`) still produce edges. Names
/// here are case-sensitive — we only filter literal lowercase forms because
/// `Get`, `Set`, etc. in TitleCase are typically real exported functions in
/// Go / C# and we do want edges for those.
pub(crate) fn is_call_stopword(name: &str) -> bool {
    matches!(
        name,
        "id"
            | "is"
            | "do"
            | "go"
            | "ok"
            | "on"
            | "to"
            | "of"
            | "as"
            | "at"
            | "be"
            | "in"
            | "if"
            | "or"
            | "no"
            | "so"
            | "up"
            | "by"
            | "it"
            | "add"
            | "all"
            | "any"
            | "are"
            | "end"
            | "for"
            | "get"
            | "has"
            | "had"
            | "key"
            | "len"
            | "map"
            | "max"
            | "min"
            | "new"
            | "not"
            | "now"
            | "out"
            | "put"
            | "raw"
            | "run"
            | "set"
            | "sum"
            | "top"
            | "use"
            | "val"
            | "var"
            | "was"
            | "who"
            | "yes"
    )
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
        let advance = idx
            + search[idx..]
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
        if idx > 0
            && let Some(prev_char) = search[..idx].chars().last()
            && (prev_char == '.'
                || prev_char == ':'
                || prev_char == '_'
                || prev_char == '$'
                || prev_char.is_ascii_alphanumeric())
        {
            search = &search[advance..];
            continue;
        }

        // Reject declaration contexts: `function name(`, `def name(`, `fn name(`,
        // `const name(`, `class name(` — these define the symbol, not call it.
        // Check if the text immediately before `name` ends with a declaration keyword.
        let before = search[..idx].trim_end();
        let is_declaration = [
            "function",
            "def",
            "fn",
            "const",
            "let",
            "var",
            "class",
            "async function",
            "async def",
            "async fn",
            "pub fn",
            "pub async fn",
        ]
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
        assert!(
            names.is_empty(),
            "namespace imports should return empty, got {:?}",
            names
        );
    }

    #[test]
    fn test_parse_es6_type_only_import_returns_empty() {
        // `import type { Foo }` is erased at runtime — no callable value created.
        let names = parse_imported_names("import type { MyType } from './types'");
        assert!(
            names.is_empty(),
            "type-only import should return empty, got {:?}",
            names
        );
    }

    #[test]
    fn test_parse_es6_malformed_named_import_does_not_panic() {
        let names = parse_imported_names("import { from './fresh-module'");
        assert!(
            names.is_empty(),
            "malformed import should be skipped, got {names:?}"
        );
    }

    #[test]
    fn test_parse_es6_malformed_mixed_import_does_not_emit_default() {
        let names = parse_imported_names("import Client, { from './fresh-module'");
        assert!(
            names.is_empty(),
            "partially extracted import should be skipped, got {names:?}"
        );
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
        assert!(
            names.is_empty(),
            "bare Python import should return empty, got {:?}",
            names
        );
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
    // extract_call_sites tests — string / comment / declaration skipping
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_call_sites_skips_double_quoted_string() {
        // `parse_imported_names("foo")` looks like a call to `parse_imported_names`,
        // but the literal `"foo"` must not register `foo` as a callee.
        let body = "let s = \"render(component)\"; helper(x);";
        let calls = extract_call_sites(body);
        assert!(calls.contains("helper"), "got {:?}", calls);
        assert!(!calls.contains("render"), "render inside a string must not register, got {:?}", calls);
    }

    #[test]
    fn test_extract_call_sites_skips_line_comment() {
        let body = "// helper(x) is intentionally commented out\nrender(c);";
        let calls = extract_call_sites(body);
        assert!(calls.contains("render"), "got {:?}", calls);
        assert!(!calls.contains("helper"), "call inside a // comment must not register, got {:?}", calls);
    }

    #[test]
    fn test_extract_call_sites_skips_block_comment() {
        let body = "/* helper(x); */ render(c);";
        let calls = extract_call_sites(body);
        assert!(calls.contains("render"), "got {:?}", calls);
        assert!(!calls.contains("helper"), "call inside /*…*/ must not register, got {:?}", calls);
    }

    #[test]
    fn test_extract_call_sites_skips_python_hash_comment() {
        let body = "def f():\n    # helper(x)\n    render(c)";
        let calls = extract_call_sites(body);
        assert!(calls.contains("render"), "got {:?}", calls);
        assert!(!calls.contains("helper"), "call inside # comment must not register, got {:?}", calls);
    }

    #[test]
    fn test_extract_call_sites_skips_template_literal_string() {
        let body = "const s = `helper(${x})`; render(c);";
        let calls = extract_call_sites(body);
        assert!(calls.contains("render"), "got {:?}", calls);
        // The simple lexer treats the entire backtick literal as a string,
        // including the `${…}` interpolation. That's fine: the worst case is
        // dropping a real call inside a template expression — never a false
        // positive. helper does not get a phantom edge.
        assert!(!calls.contains("helper"), "call inside `…` template must not register, got {:?}", calls);
    }

    #[test]
    fn test_extract_call_sites_handles_escaped_quotes() {
        // Escaped quote must not end the string early; the inner `helper(x)`
        // stays inside the string region and must not register.
        let body = "let s = \"abc \\\" helper(x) \\\" def\"; render(c);";
        let calls = extract_call_sites(body);
        assert!(calls.contains("render"), "got {:?}", calls);
        assert!(!calls.contains("helper"), "escaped-quote inner must remain string-context, got {:?}", calls);
    }

    #[test]
    fn test_extract_call_sites_drops_stopwords() {
        let body = "get(x); set(y); render(c);";
        let calls = extract_call_sites(body);
        assert!(calls.contains("render"), "got {:?}", calls);
        assert!(!calls.contains("get"), "stopword 'get' must not register, got {:?}", calls);
        assert!(!calls.contains("set"), "stopword 'set' must not register, got {:?}", calls);
    }

    #[test]
    fn test_extract_call_sites_keeps_short_non_stopwords() {
        // `tick` / `init` / `save` are 4 chars but were dropped by the old
        // blanket length filter; the stopword approach keeps them.
        let body = "tick(); init(); save();";
        let calls = extract_call_sites(body);
        assert!(calls.contains("tick"), "got {:?}", calls);
        assert!(calls.contains("init"), "got {:?}", calls);
        assert!(calls.contains("save"), "got {:?}", calls);
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
        assert!(!body_contains_call(
            "function helper(x) { return x; }",
            "helper"
        ));
        // `def helper(` — Python declaration
        assert!(!body_contains_call(
            "def helper(x):\n    return x",
            "helper"
        ));
        // `fn helper(` — Rust declaration
        assert!(!body_contains_call(
            "fn helper(x: i32) -> i32 { x }",
            "helper"
        ));
        // Actual call SHOULD match
        assert!(body_contains_call("let result = helper(42);", "helper"));
    }

    // -----------------------------------------------------------------------
    // import_calls_pass integration tests
    // -----------------------------------------------------------------------

    fn extract_python_nodes(path: &str, source: &str) -> Vec<Node> {
        use crate::extract::configs::PYTHON_CONFIG;
        use crate::extract::generic::GenericExtractor;

        let mut nodes = GenericExtractor::new(&PYTHON_CONFIG)
            .run(std::path::Path::new(path), source)
            .unwrap()
            .nodes;
        for node in &mut nodes {
            node.id.root = "r".into();
        }
        nodes
    }

    #[test]
    fn generic_scoped_method_lexical_alias_emits_canonical_cross_file_reference() {
        let mut nodes = extract_python_nodes(
            "caller.py",
            "def orchestrate(worker):\n    return worker.execute()\n",
        );
        nodes.extend(extract_python_nodes(
            "worker.py",
            "class Worker:\n    def execute(self):\n        return 1\n",
        ));

        let caller = nodes
            .iter()
            .find(|node| node.id.name == "orchestrate")
            .unwrap();
        assert_eq!(caller.metadata.get("attr_refs").map(String::as_str), Some("execute"));
        let callee = nodes
            .iter()
            .find(|node| node.id.name == "Worker.execute")
            .unwrap();
        assert_eq!(
            callee.metadata.get("lexical_name").map(String::as_str),
            Some("execute")
        );

        let edges = import_calls_pass(&nodes);
        let references = edges
            .iter()
            .filter(|edge| {
                edge.kind == EdgeKind::ReferencedBy
                    && edge.from == caller.id
                    && edge.to == callee.id
            })
            .collect::<Vec<_>>();
        assert_eq!(references.len(), 1, "expected canonical qualified target: {edges:?}");
    }

    #[test]
    fn generic_duplicate_scoped_method_lexical_alias_fails_closed() {
        let mut nodes = extract_python_nodes(
            "caller.py",
            "def orchestrate(worker):\n    return worker.execute()\n",
        );
        nodes.extend(extract_python_nodes(
            "worker.py",
            "class Worker:\n    def execute(self):\n        return 1\n",
        ));
        nodes.extend(extract_python_nodes(
            "other.py",
            "class Other:\n    def execute(self):\n        return 2\n",
        ));
        let caller = nodes
            .iter()
            .find(|node| node.id.name == "orchestrate")
            .unwrap();

        let edges = import_calls_pass(&nodes);
        assert!(
            edges.iter().all(|edge| {
                edge.kind != EdgeKind::ReferencedBy || edge.from != caller.id
            }),
            "ambiguous bare method alias must not choose an arbitrary owner: {edges:?}"
        );
    }

    #[test]
    fn test_cross_file_call_emitted() {
        // Caller in file A imports `helper` from file B, calls it.
        let caller = make_fn("a.ts", "main", "function main() { return helper(42); }");
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");
        let import = make_import("a.ts", "import { helper } from './b'");

        let nodes = vec![caller.clone(), callee.clone(), import];
        let edges = import_calls_pass(&nodes);

        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert_eq!(calls.len(), 1, "expected 1 Calls edge, got {:?}", edges);
        assert_eq!(calls[0].from.name, "main");
        assert_eq!(calls[0].to.name, "helper");
        assert_eq!(calls[0].kind, EdgeKind::Calls);
        // Also expect a ReferencedBy edge from the import
        let refs: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ReferencedBy).collect();
        assert_eq!(refs.len(), 1, "expected 1 import ReferencedBy edge");
    }

    #[test]
    fn test_no_edge_when_no_import() {
        // `helper` appears in the body, but there's no import statement.
        let caller = make_fn("a.ts", "main", "function main() { return helper(42); }");
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");

        let nodes = vec![caller, callee];
        let edges = import_calls_pass(&nodes);

        assert!(edges.is_empty(), "no import → no edge, got {:?}", edges);
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
        // No Calls edge (helper not called in body), but import creates a ReferencedBy edge
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert!(calls.is_empty(), "no call in body → no Calls edge, got {:?}", calls);
        let refs: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ReferencedBy).collect();
        assert_eq!(refs.len(), 1, "import creates ReferencedBy even without body call");
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

        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert_eq!(calls.len(), 1, "expected 1 Python Calls edge, got {:?}", edges);
        assert_eq!(calls[0].from.name, "get_workspace");
        assert_eq!(calls[0].to.name, "fetch_data");
    }

    #[test]
    fn test_no_self_edge() {
        // A function that imports itself (unusual, but guard defensively).
        let node = make_fn(
            "a.ts",
            "processData",
            "function processData() { return processData(); }",
        );
        let import = make_import("a.ts", "import { processData } from './b'");
        // No other file defines processData — so no edge anyway.

        let nodes = vec![node, import];
        let edges = import_calls_pass(&nodes);

        for e in &edges {
            assert_ne!(e.from, e.to, "self-edge must never be emitted");
        }
    }

    #[test]
    fn test_stopword_names_skipped() {
        // Imported names that are common short builtins (`get`, `set`, `id`)
        // are dropped via the stopword deny-list — they otherwise create
        // false-live edges across every same-named accessor in the workspace.
        let caller = make_fn("a.ts", "main", "function main() { get(x); }");
        let callee = make_fn("b.ts", "get", "function get(x) { return x; }");
        let import = make_import("a.ts", "import { get } from './b'");

        let nodes = vec![caller, callee, import];
        let edges = import_calls_pass(&nodes);

        assert!(
            edges.is_empty(),
            "stopword names must be skipped, got {:?}",
            edges
        );
    }

    #[test]
    fn test_short_non_stopword_names_emit_edges() {
        // `init` is 4 chars but historically dropped under the old `len() < 4`
        // gate when typed as `tick` / `save` / etc. Removing the gate in favour
        // of a stopword list lets these legitimate short calls produce edges.
        let caller = make_fn("a.ts", "main", "function main() { init(x); }");
        let callee = make_fn("b.ts", "init", "function init(x) { return x; }");
        let import = make_import("a.ts", "import { init } from './b'");

        let nodes = vec![caller, callee, import];
        let edges = import_calls_pass(&nodes);

        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert_eq!(
            calls.len(),
            1,
            "non-stopword short name must emit a Calls edge, got {:?}",
            edges
        );
    }

    #[test]
    fn test_method_call_not_confused_with_bare_call() {
        // `obj.helper()` must not emit a Calls edge for `helper`.
        let caller = make_fn("a.ts", "main", "function main() { return obj.helper(42); }");
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");
        let import = make_import("a.ts", "import { helper } from './b'");

        let nodes = vec![caller, callee, import];
        let edges = import_calls_pass(&nodes);

        // No Calls edge (method call, not bare call), but import ReferencedBy still emitted
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert!(calls.is_empty(), "method call `obj.helper()` must not emit Calls edge, got {:?}", calls);
        // Import ReferencedBy is still emitted since helper is imported
        let refs: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::ReferencedBy).collect();
        assert_eq!(refs.len(), 1, "import ReferencedBy still emitted");
    }

    #[test]
    fn test_relative_import_gets_detected_confidence() {
        let caller = make_fn("a.ts", "main", "function main() { return helper(42); }");
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");
        let import = make_import("a.ts", "import { helper } from './b'");

        let nodes = vec![caller, callee, import];
        let edges = import_calls_pass(&nodes);

        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].confidence, Confidence::Detected, "relative import should produce Detected confidence");
    }

    #[test]
    fn test_non_relative_import_also_emits_edge() {
        let caller = make_fn("a.ts", "main", "function main() { return helper(42); }");
        let callee = make_fn("b.ts", "helper", "function helper(x) { return x; }");
        // Non-relative import but matching function exists in local graph
        let import = make_import("a.ts", "import { helper } from 'some-library'");

        let nodes = vec![caller, callee, import];
        let edges = import_calls_pass(&nodes);

        // Calls edge + import ReferencedBy edge
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert_eq!(calls.len(), 1, "expected 1 Calls edge for the non-relative import");
        for e in &edges {
            assert_eq!(e.confidence, Confidence::Detected, "all import-calls edges use Detected confidence");
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
            line_start: 1,
            line_end: 3,
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
            line_start: 1,
            line_end: 3,
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
            line_start: 1,
            line_end: 5,
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
            line_start: 1,
            line_end: 3,
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
            line_start: 1,
            line_end: 1,
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
            line_start: 1,
            line_end: 5,
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
            line_start: 1,
            line_end: 3,
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
            line_start: 1,
            line_end: 1,
            signature: "import { helperB } from './helpers'".into(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let nodes = vec![
            caller_a,
            callee_a.clone(),
            import_a,
            caller_b,
            callee_b.clone(),
            import_b,
        ];
        let edges = import_calls_pass(&nodes);

        // Expected: 2 Calls edges — mainA→helperA (root-a) and mainB→helperB (root-b).
        // Plus 2 import ReferencedBy edges.
        let calls: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Calls).collect();
        assert_eq!(
            calls.len(),
            2,
            "expected 2 isolated cross-file Calls edges, got {:?}",
            calls.iter().map(|e| format!("{}->{}", e.from.name, e.to.name)).collect::<Vec<_>>()
        );

        let edge_a = calls.iter().find(|e| e.from.root == "root-a");
        assert!(edge_a.is_some(), "missing root-a edge");
        assert_eq!(edge_a.unwrap().from.name, "mainA");
        assert_eq!(edge_a.unwrap().to.name, "helperA");

        let edge_b = calls.iter().find(|e| e.from.root == "root-b");
        assert!(edge_b.is_some(), "missing root-b edge");
        assert_eq!(edge_b.unwrap().from.name, "mainB");
        assert_eq!(edge_b.unwrap().to.name, "helperB");
    }
}
