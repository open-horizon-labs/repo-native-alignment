---
date: "2026-07-15"
pipeline_issue: "/ship PR #747"
pr: 747
phase: ship
---

# PR #747 ship friction

| Date | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-15 | RNA worktree search | minor | The new #735 worktree had no persisted Lance cache, so RNA could not query it yet. | Queried the adjacent, already-indexed #734 worktree for unchanged code and test context without falling back to a source scan. | Make a new worktree immediately queryable from its parent branch index. |
| 2026-07-15 | RNA JSON artifact body | moderate | The #734 RNA index returned the pre-migration RustSec policy body after the branch had moved, which would have reintroduced a removed `lru` record if trusted. | Diagnosed the stale index explicitly and used `git show issue/734:<path>` for the authoritative committed policy and fixtures. | Attach indexed artifact content to its commit identity and surface staleness before returning bodies. |
| 2026-07-15 | metal-candle prerequisite PR #6 | moderate | The repo exposed no Actions runs for the fork branch, and its first merge attempt conflicted with a newly merged parent PR. | Rebased onto current `main`, reran Rust 1.88 all-feature checks and all 457 executable tests locally, then merged the prerequisite after real CPU/Metal output parity passed. | Enable fork-branch CI and require the verified Rust floor in metal-candle. |
