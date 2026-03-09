# Session: graph-quality-107

## Aim
**Updated:** 2026-03-09

**Aim:** graph_query and search_symbols return results that agents can actually use for impact analysis and code exploration — not module nodes and PR merges.

**Current State:** Field testing on Innovation-Connector (Python/TypeScript monorepo, 43K symbols):
- graph_query impact on `ensure_user` → returns only containing module, not callers
- graph_query neighbors on TypeScript import → no results
- search_symbols "User" → imports, tests, markdown before core implementation
- oh_search_context → "Table not found" (fixed by #106 symlink crash)

**Desired State:**
- graph_query impact shows actual callers across files
- graph_query neighbors on imports resolves to target module
- search_symbols ranks exact name match > signature > partial
- oh_search_context works on any repo (no crashes on symlinks)

## Problem Space

Tree-sitter extracts within-file structure. Cross-file edges require either LSP (Phase 2, background) or import path resolution (doable at extraction time). Without cross-file edges, graph_query is useless for impact analysis.

## Solution Space
**Updated:** 2026-03-09

Three independent fixes, all needed:

### A: Filter noise from graph_query
- Remove module and PR-merge nodes from graph_query display results
- These are structural scaffolding, not useful for agents
- Location: `format_neighbor_nodes` in server.rs

### B: Better search ranking
- Sort: exact name match first, then signature contains, then partial
- Currently no ranking — results come in scan order
- Location: search_symbols handler in server.rs

### C: Import path resolution
- At extraction time, resolve `import { X } from './util/user_utils'` to a file path
- Create `DependsOn` edge from importing file to target file
- Doesn't require LSP — just path resolution
- Location: extract/generic.rs import handling

## Execute Status
- Branch: `fix-107-graph-search-quality`
- PR: #109 — review/dissent posted
- **Done:** A (filter noise), B (search ranking), C (import resolution code)
- **Pending:** C end-to-end verification blocked by lance panic (#110)
- Lance panic on 10K+ embedding batch filed as #110

## Related
- #105 — symlink crash (merged via #106)
- #107 — this issue
- #109 — PR with fixes A+B+C
- #110 — lance panic blocking C verification
- #108 — Claude Code memory: CLAUDE.md already indexed, auto memory cross-reference only
- #90 — parent DX epic (closed, but quality is DX)
