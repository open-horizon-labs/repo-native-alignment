---
id: suffix-match-import-resolution
outcome: agent-alignment
title: Suffix match resolves cross-file imports without language-specific logic in the graph builder
---

## The Problem

Tree-sitter extracts import statements but can't resolve them to target files — it doesn't have `sys.path`, `tsconfig.json`, or module resolution context. Without cross-file edges, `graph_query` impact analysis returns only the containing module, not actual callers.

## The Insight

Import paths are suffixes of file paths. `from src.util.user_utils import ensure_user` maps to `src/util/user_utils.py`, which is a suffix of `ai_service/src/util/user_utils.py`. The graph builder already has the full list of scanned files.

**Architecture:** Extractors emit best-effort paths (dots→slashes for Python, relative paths for TypeScript). The graph builder resolves dangling edges against the scanned file index via suffix match. One pass, language-agnostic, O(edges × files) but fast in practice because the set lookup is O(1).

## Why This Matters

- `ensure_user` impact analysis went from 1 result (containing module) to 6 results (module + 2 real callers + their transitive dependents)
- Works across Python (absolute and relative imports), TypeScript, and any language where import paths map to file path substrings
- No language-specific logic in the graph builder — the extractor handles language quirks, the builder just matches suffixes
- Ambiguous matches (0 or 2+) are left as-is — no false connections

## The Pattern

Extractors are best-effort, builders are resolvers. Don't try to be perfect at extraction time — emit what you know, let the phase with more context (the builder, which sees all files) fix what it can.
