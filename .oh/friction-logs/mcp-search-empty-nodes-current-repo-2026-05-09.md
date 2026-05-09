---
id: mcp-search-empty-nodes-current-repo-2026-05-09
outcome: context-assembly
severity: major
---

# MCP search empty nodes/current repo friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-05-09 | `mcp_rna_server_search` with generated defaults | major | The harness supplied `nodes: []` and `repo: <current repo>`. The server interpreted empty `nodes` as batch mode and returned `Empty nodes list`, and interpreted the current repo path as an external persisted LanceDB query instead of the live in-memory graph. | Incremental mutations were extracted but not observable through MCP tools using the generated call shape; agents could falsely conclude incremental indexing was still broken. | Behavior changed so empty `nodes`/blank fields are absent, and blank/current repo paths use live graph requests where foreground incremental snapshots are visible. |
