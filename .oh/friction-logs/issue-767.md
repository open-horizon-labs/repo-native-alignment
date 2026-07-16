---
title: Issue 767 RNA dogfood friction
date: 2026-07-16
issue: 767
outcome: context-assembly
---

# Issue 767 RNA dogfood friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-16 | Codex RNA MCP discovery | moderate | The dedicated issue-agent session did not expose RNA MCP `search`, `repo_map`, or `outcome_progress` tools even though the repository requires them. | Exploration used the installed `repo-native-alignment` CLI service path instead of MCP. | Verify repo-local MCP tools propagate into dedicated sub-agent sessions. |
| 2026-07-16 | `repo-native-alignment search --nodes ... --include-body` | moderate | Multiple Rust implementations with the same function name share a colliding node ID; requesting `src/extract/lsp/mod.rs:enrich:function` returned `DummyEnricher::enrich` instead of `LspEnricher::enrich`. The CLI also has no bounded file/line retrieval parameters. | Exact implementation context requires bounded `sed` reads after graph-first exploration. | Preserve parent-qualified identities for impl methods and expose the MCP file/line retrieval seam in CLI parity. |
| 2026-07-16 | `repo-native-alignment search "LSP query profile"` | low | The installed RNA index returned the removed `LspEligibilityPolicy` after the working tree had replaced it, so review search results were stale. | Review used the branch diff and bounded source reads for changed code rather than trusting stale graph results. | Make working-tree freshness explicit in search output and provide a targeted refresh path. |
