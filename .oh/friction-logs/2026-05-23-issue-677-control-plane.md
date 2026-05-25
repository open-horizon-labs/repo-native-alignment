---
id: 2026-05-23-issue-677-control-plane
outcome: context-assembly
severity: minor
---

# Issue #677 control-plane pass friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-05-23 | RNA MCP search against fresh worktree `/Users/muness1/src/open-horizon-labs/rna-issue-677` | minor | The worktree had no persisted `.oh/.cache/lance` graph, so RNA MCP returned `No persisted graph`; code exploration had to use the already-indexed main worktree for graph queries and source reads in the feature worktree for edit anchors. | No product behavior impact, but fresh worktree workflows are not immediately dogfoodable through RNA without an explicit scan/cache warmup. | Consider worktree prep that copies or warms RNA's `.oh/.cache/lance`, or make MCP search surface a one-command scan hint that preserves the dogfood path. |
