---
id: 674-ship
outcome: context-assembly
severity: skipped
---

# PR #674 ship friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-05-07 | `mcp_rna_server_search` artifact search | skipped | Searching for known guardrail/metis IDs (`computed-but-not-delivered`, `dogfood-rna-tools`, `subagent-prompts-require-rna-directive`) returned no results even though `.oh/guardrails/*.md` files exist and are required by ship preflight. | Had to use `find`/`read` fallback to inspect ship guardrails. | Investigate artifact indexing/search recall for exact guardrail IDs and paths. |
| 2026-05-07 | `repo-native-alignment search "" --repo . --limit 1` scan gate | minor | Ship scan-gate snippet did not produce the expected `N symbols` count for an empty query and fell through to a full scan attempt, which timed out in the harness. | Added an explicit bounded `scan --extract-only --no-embed --no-lsp` to refresh the worktree index before review. | Update ship scan-gate instructions to use a bounded readiness command or robust output parsing. |
