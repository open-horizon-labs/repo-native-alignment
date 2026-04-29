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
| 2026-04-29 | `mcp_rna_server_search` | Keyword search for CodeRabbit follow-up symbols (`parse_cargo_toml`, `refresh_manifest_graph`, `update_graph_with_scan`, `await_background_embed`) returned no results in the live worktree. | Used targeted `read`, `grep`, and LSP where available to inspect exact CodeRabbit-commented sections. | Added one more dogfood gap during final ship sweep; implementation remained grounded in reviewed source ranges. |
| 2026-04-29 | `mcp_rna_server_search` | Artifact/session query for #659 capability-scoped enrichment readiness timed out after 60s. | Used targeted `read` of `.oh/sessions/659-capability-scoped-enrichment-readiness.md` and GitHub issue views. | Session continuation still grounded, but RNA artifact lookup was unavailable for this handoff. |
| 2026-04-29 | `mcp_rna_server_search` | Readiness implementation queries for `EnrichmentStatus`/status surfaces timed out after 60s twice. | Used targeted `grep` and `read` for `src/server/state.rs`, `src/server/helpers.rs`, `src/service/search.rs`, and the dead-code skill. | Dogfood gap: RNA could not support its own readiness/control-plane implementation discovery in-session. |
