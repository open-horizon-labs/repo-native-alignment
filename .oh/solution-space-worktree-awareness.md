---
issue: 42
title: "RNA worktree awareness — root-scoped IDs + auto-detected worktree roots"
status: design
date: 2026-03-08
outcome: agent-alignment
---

# Solution Space: RNA Worktree Awareness (Issue #42)

## Problem

RNA MCP server is a single process pointed at one repository path. When agents
run in git worktrees, `search_symbols` returns main-branch symbols — not the
agent's in-progress edits. Business context queries (metis/guardrails) work
correctly; code navigation is stale because the scanner only knows about the
primary repo root.

## Root Cause of the Naive Fix

The obvious approach — spin up a second RNA process per worktree — fails because
the same file at different states produces two nodes with the same `NodeId`.
LanceDB upserts are keyed on `to_stable_id()`, which uses the format
`{root}:{file}:{name}:{kind}`. With an empty or identical `root`, main and
worktree versions of the same function collide on write.

## Recommended Approach: Root-Scoped IDs + Auto-Detected Worktree Roots

### What already exists (do not re-implement)

- `NodeId` already has a `root: String` field (`src/graph/mod.rs:24`).
- `to_stable_id()` already prefixes with `root` (`src/graph/mod.rs:37-45`).
- `WorkspaceConfig` / `RootConfig` / `ResolvedRoot` already model multi-root
  scanning (`src/roots.rs`).
- `build_full_graph` already iterates `resolved_roots`, assigns `root_slug` to
  every node and edge (`src/server.rs:944-960`).
- The LanceDB `symbols` and `edges` tables already store a `root_id` column
  (`src/graph/store.rs:18,70`).

### What is missing

1. **Worktree auto-detection at scanner startup** — `WorkspaceConfig` only reads
   `~/.config/rna/roots.toml` and the `--repo` CLI flag. It has no mechanism to
   discover active git worktrees.

2. **Background scanner only covers the primary root** — the 15-minute
   background loop (`src/server.rs:820-866`) creates `Scanner::new(repo_root)`
   directly, bypassing the multi-root workspace config entirely. Worktree roots
   would never be re-scanned incrementally.

3. **No node-deletion by root prefix** — when a worktree is removed, there is
   no path that executes `DELETE WHERE root_id = '<worktree-slug>'` against
   LanceDB. Stale worktree nodes linger.

4. **Slug collision risk for worktrees** — `path_to_slug` uses only the
   directory basename (`src/roots.rs:228-236`). Two worktrees of the same repo
   checked out as `feat/foo` and `feat/bar` would yield directory names
   `agent-XXXXX` and `agent-YYYYY` (typical worktree paths under
   `.git/worktrees/`), but the slug must be derived from the full worktree path
   to guarantee uniqueness.

## Implementation Plan (in order)

### Step 1 — `src/roots.rs`: `WorkspaceConfig::with_worktrees(repo_root)`

Add a method that reads `.git/worktrees/` (or runs `git worktree list
--porcelain`) relative to the primary repo root and appends one `RootConfig`
per active worktree:

```
pub fn with_worktrees(mut self, repo_root: &Path) -> Self
```

- Parse `.git/worktrees/<name>/gitdir` files — each contains the absolute path
  of the worktree checkout.
- Skip worktrees whose `gitdir` points back to the primary root.
- Add each as `RootConfig { root_type: CodeProject, git_aware: true, ... }`.
- Slug must be derived from the **full worktree path** (not just the basename)
  to avoid collisions.

### Step 2 — `src/server.rs`: chain `with_worktrees` into startup

In `build_full_graph` (and `list_roots`), replace:

```rust
let workspace = WorkspaceConfig::load()
    .with_primary_root(self.repo_root.clone());
```

with:

```rust
let workspace = WorkspaceConfig::load()
    .with_primary_root(self.repo_root.clone())
    .with_worktrees(&self.repo_root);
```

The rest of the multi-root loop is unchanged — each worktree root gets its own
`root_slug`, its own state-cache path, and its own scanner instance.

### Step 3 — `src/server.rs`: background scanner covers all roots

Replace the single-root `Scanner::new(repo_root)` in the background loop with
the same `WorkspaceConfig::load().with_primary_root(...).with_worktrees(...)`
pattern, iterating all resolved roots — mirroring `build_full_graph`.

### Step 4 — `src/server.rs` or `src/graph/store.rs`: node deletion by root

Add a `delete_nodes_for_root(root_slug: &str)` helper against LanceDB:

```sql
DELETE FROM symbols WHERE root_id = '<slug>'
DELETE FROM edges   WHERE root_id = '<slug>'
```

Call this in the background scan loop when `with_worktrees` returns a set of
roots that no longer includes a previously-known worktree slug (diff the
previous root set against the current set).

### Step 5 — `src/graph/index.rs`: test helper `make_node_id`

The test helper at `src/graph/index.rs:259` uses `root: "test"`, which is
fine. No change needed — but new tests for worktree-scoped IDs should be added
alongside the implementation.

## Key Files and Their Changes

| File | Change |
|---|---|
| `src/roots.rs` | Add `WorkspaceConfig::with_worktrees(repo_root)` — reads `.git/worktrees/` |
| `src/server.rs` | Chain `with_worktrees` in `build_full_graph`, `list_roots`, and background scanner loop |
| `src/graph/store.rs` | Add `delete_nodes_for_root` / `delete_edges_for_root` helpers |
| `src/server.rs` (background loop) | Track known worktree slugs; call delete helpers when a worktree disappears |

`src/graph/mod.rs` and `src/graph/index.rs` need no changes — `NodeId.root`
and the stable ID format are already correct.

## Known Risks

### ID migration for existing indexes

Existing LanceDB rows have `root_id` set to the slug derived from the primary
repo path (e.g., `repo-native-alignment`). Worktree nodes will have different
slugs. A fresh scan after the feature lands will correctly populate worktree
nodes; no migration script is needed. Old rows remain valid.

### Slug uniqueness across worktrees of the same repo

Two worktrees of the same project may have the same directory basename. The
slug function must use more of the path (e.g., last two path components, or a
short hash of the full absolute path) to guarantee uniqueness. The current
`path_to_slug` only takes the final directory name — this must be addressed in
Step 1.

### `search_symbols` result presentation with multiple roots

When both `main::src/foo.rs::handle` and `worktree-feat-x::src/foo.rs::handle`
are returned for the same query, agents need to know which result is current
for their working context. The MCP `search_symbols` response already surfaces
the full node ID (which includes `root_id`); no schema change is required, but
the tool description should be updated to note that `root` identifies the
source (main vs. worktree).

### Background scanner frequency vs. worktree churn

Worktrees can be created and deleted faster than the 15-minute scan interval.
A freshly-created worktree will be invisible to RNA until the next background
scan or the next tool call that triggers `get_graph`. This is acceptable for
the initial implementation — the on-demand path (`get_graph` on first tool
call) will pick up new worktrees within one request cycle.

## What This Explicitly Does NOT Do

- **No separate RNA process per worktree.** Single process, shared DB,
  root-scoped writes.
- **No DB copy at worktree creation.** CoW / hardlinks (Option D, macOS APFS)
  are not needed because IDs are namespaced — no collision risk.
- **No manual `rna root add` command.** Auto-detection via `.git/worktrees/`
  is sufficient.
- **No changes to the MCP protocol.** `search_symbols` already accepts an
  optional `root` filter parameter for callers who want to scope results.
