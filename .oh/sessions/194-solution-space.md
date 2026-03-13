---
title: "Default Search Results to Caller's Worktree Root"
issue: 194
phase: solution-space
---

# Issue #194: Default Search Results to Caller's Worktree Root

## Problem

Queries return results from ALL indexed roots by default. An agent working in worktree A sees symbols from worktrees B, C, and the primary root — polluting every search with irrelevant duplicates. The `root` filter on `search` exists but is opt-in and only on one tool.

## Solution Space

### Option A: Default to primary root slug (MVP)

`RnaHandler` already knows `self.repo_root` from `--repo`. Add an `effective_root_filter()` helper:
- `root: None` → scope to primary root slug
- `root: "all"` → no filter (cross-root search)
- `root: "some-slug"` → explicit scope

Apply to every tool handler: `search`, `repo_map`, `oh_search_context`, `outcome_progress`.

Add `root` param (optional) to `RepoMap`, `OhSearchContext`, `OutcomeProgress` structs.

**Pros:** Simple, covers 90% of cases, backward-compatible (existing callers get better results by default).
**Cons:** Agents in worktrees still see primary root, not their own worktree.

### Option B: Detect caller's worktree at startup

If `--repo` points inside a worktree (or the server is started from one), resolve to that worktree's slug instead of the primary root. Same `effective_root_filter()` logic as A but the default changes based on launch context.

**Pros:** Worktree agents get correct scoping automatically.
**Cons:** Slightly more complex startup logic. Need to detect if CWD is inside a worktree vs primary.

### Option C: Per-request root inference via MCP

Pass a `root_hint` in MCP initialize params so the client can tell the server which workspace it's in. MCP doesn't natively support this, but we control the server.

**Pros:** Most accurate — client knows exactly where it is.
**Cons:** Requires client-side changes. Not available in all MCP clients.

## Recommendation

**Start with A, layer B on top.** A is a one-session fix. B is a small addition (resolve `--repo` through git worktree detection). C is future work if MCP evolves to support it.

## Non-code roots

Notes roots (Claude memory, `.oh/` artifacts) should always be included regardless of `root` filter — they're context, not code. The scoping should only apply to `CodeProject` type roots.

The `root` filter should work like:
- Code symbols: filtered to effective root
- `.oh/` artifacts (outcomes, guardrails, metis): always included
- Claude memory (notes root): always included
- Commits: always included (they're repo-wide)

## Implementation sketch

1. Add `primary_root_slug()` and `effective_root_filter()` to `RnaHandler`
2. Add `root: Option<String>` to `RepoMap`, `OhSearchContext`, `OutcomeProgress` structs
3. Wire `effective_root_filter()` into all handler functions
4. Non-code roots (Notes type) bypass the filter
5. Update tool descriptions to document `root` param and "all" escape hatch

## Dependencies

- #198 (stale root pruning) — fixes data layer, this fixes query layer
- #199 (empty root slugs) — must land first or primary root symbols won't match the filter
