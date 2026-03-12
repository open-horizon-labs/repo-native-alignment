# Friction Log: #143 Test Coverage Mapping

## Summary

One RNA tool friction event. One tooling/environment friction event (not RNA-specific).

## Friction Events

### 1. search_symbols: enum variants not indexed
- **Phase:** 1 (Problem Statement)
- **Tool:** search_symbols
- **Severity:** Low
- **What happened:** Searching for `Calls` with `kind=enum` returned no results, even though `EdgeKind::Calls` exists at `src/graph/mod.rs:136`. Enum variants (the individual arms of an enum) are not indexed as standalone symbols -- only the enum itself is indexed.
- **Workaround:** Read the file directly to find the EdgeKind definition.
- **Recommendation:** Known limitation. Enum variants would need to be emitted as separate symbols (kind=variant or kind=const) during tree-sitter extraction. Low priority since the enum itself is findable.

### 2. Edit tool path confusion in worktrees (not RNA)
- **Phase:** 3 (Execute)
- **Tool:** Claude Edit tool (not RNA)
- **Severity:** Medium
- **What happened:** The Edit tool applied changes to the main repo's `src/server.rs` instead of the worktree copy. Since worktrees have separate working directories, the changes were invisible to the worktree's cargo build. This caused ~15 minutes of debugging "why aren't my tests showing up."
- **Workaround:** Manually copied the file to the worktree, then used explicit worktree paths for all subsequent edits.
- **Recommendation:** Not an RNA issue. This is a Claude Code / worktree interaction issue.
