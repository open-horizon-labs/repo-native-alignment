---
pr: 661
outcome: context-assembly
severity: skipped
---

# PR #661 Ship Friction

| Time | Tool/Path | Friction | Fallback | Impact |
|------|-----------|----------|----------|--------|
| 2026-04-28 | `mcp_rna_server_search` | Broad artifact query for review-readiness/manifest/LSP returned no results despite relevant session/guardrail artifacts existing. | Retried narrower RNA artifact searches, then used targeted `read` for session/skill context. | Slowed ship review; evidence still grounded. |
| 2026-04-28 | `mcp_rna_server_search(include_body=true)` | File-scoped include-body requests failed because the tool requires explicit `node`/`nodes`; file-only review could not retrieve bodies through RNA. | Used RNA compact symbol listing, then targeted `read` ranges for changed functions. | Friction event under dogfood-rna-tools. |
| 2026-04-28 | LSP references | `lsp references` for `update_graph_with_scan` aborted. | Used RNA search, ast-grep, then targeted grep for callsites. | Needed to verify signature-change callsites before editing. |
| 2026-04-28 | `mcp_rna_server_search` | Keyword search for `refresh_manifest_graph` returned no result while the symbol existed in the working tree. | Used ast-grep identifier lookup and targeted read. | Indicates index lag/coverage gap for fresh working-tree symbols. |
