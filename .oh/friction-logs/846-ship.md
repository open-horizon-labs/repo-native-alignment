---
type: friction-log
issue: 833
pr: 846
status: active
date: 2026-07-31
---

# PR #846 ship + v0.2.11 release friction

RNA path used first for every navigation question. Scan gate passed before any
source read: the worktree index reported 52,508 symbols, schema v25, "last scan
just now", so no rescan was needed.

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-31 | RNA MCP `search` (flat) | none | Symbol lookup for `LspWorkIdentity` and `lsp_toolchain_contract` returned exact hits with stable node IDs on the first query. | No fallback needed. | — |
| 2026-07-31 | RNA MCP `search` with `file`+`line`/`end_line` | none | Bounded current-filesystem source retrieval served every "show me this exact contract" question (toolchain contract, `identity_disposition`, completeness selection). Provenance line correctly warned that the span is current filesystem state, not the indexed snapshot. | Replaced what would otherwise have been four `Read` calls. | — |
| 2026-07-31 | RNA impact/neighbors → bounded `grep` fallback | minor | Verifying the reviewer findings required enumerating **call sites of private module-level functions** (`record_integrity_digest`, `write_store`). The worktree capability report states `lsp_call_references: unavailable`, so the extract-only graph carries no `Calls` edges and an impact query cannot answer "who calls this". Same root cause already logged in `833-execute.md`. | Used bounded single-file `grep` to enumerate the two call sites, then returned to RNA source spans to read them. Two friction events. | Surface syntax-level same-file/same-crate references when LSP call/reference edges are unavailable — this is the third session in a row blocked on the same gap. |
| 2026-07-31 | RNA search → bounded `grep` for in-file mention sweeps | minor | Questions of the form "every mention of `planner_contract` / `integrity_digest` / `prior_node_id_counts` in this one file" are not answerable by symbol search, and AGENTS.md deliberately excludes function-body matching as noise. Field-level nodes exist but do not carry their in-body use sites. | Used bounded per-file `grep` for six mention sweeps, scoped to files RNA had already identified. Six friction events. | Consider a scoped `mentions` mode (single file or single symbol's body) distinct from ranked code search, so verification sweeps do not have to leave RNA. |
| 2026-07-31 | Harness `Read` before `Edit` | n/a | The Edit tool requires a prior `Read` of the exact file. Not an RNA gap. | Two `Read` calls on already-RNA-located line ranges. | — |
| 2026-07-31 | `rustfmt` single file | minor | Same pre-existing drift noted in `833-execute.md`: workspace-wide `cargo fmt` reformats unrelated files. | Ran `rustfmt` on the one touched file only. | Unchanged: add a changed-hunk formatting gate or normalize in a dedicated change. |

**Total friction events:** 8 (2 impact-query gaps, 6 in-file mention sweeps).
**Recurring theme:** short-lived fix worktrees never have LSP enrichment
attached, so every ship/review pass in a worktree loses caller/reference
navigation and falls back to bounded text search for exactly the questions a
reviewer asks most.
