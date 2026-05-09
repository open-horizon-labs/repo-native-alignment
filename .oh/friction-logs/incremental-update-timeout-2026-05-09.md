---
id: incremental-update-timeout-2026-05-09
outcome: context-assembly
severity: major
---

# MCP incremental update timeout friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-05-09 | `mcp_rna_server_search` after editing a one-file Rust probe | major | Exact search for the modified symbol timed out after 60s while the background incremental LSP pipeline held `graph_build_lock`. Foreground incremental refresh was waiting on the same lock even though a fast tree-sitter snapshot should be enough for MCP search. | User-visible MCP tools appeared hung for a trivial incremental edit; had to inspect logs and source with fallback `read`/`grep` while the MCP server was unresponsive. | Fixed in working tree: `get_graph` now uses non-blocking `try_lock` for foreground incremental refresh and stale background pipelines skip commit/persist/swap if a newer fast snapshot appears. Regression test and real MCP probe added/run. |
