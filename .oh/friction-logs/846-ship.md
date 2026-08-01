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
| 2026-07-31 | Step 10c reviewer — RNA MCP `search` with `file`+`line`/`end_line` | none | Bounded current-filesystem retrieval answered every "show me the exact contract" question for the independent review (`begin` disposition state machine, `LspWorkItemRecord` fields, `load_store`/`write_store`, `select_work_items_for_report`, `partition_identities`, `Node`/`Edge` shape). | Six bounded spans replaced six `Read` calls. Zero friction. | — |
| 2026-07-31 | Step 10c reviewer — RNA CLI `search --compact` | none | Worktree binary confirmed the live index (52,508 symbols) and resolved `LspWorkIdentity`, `merge_and_write_store`, `maybe_flush`, `store_path` with stable node IDs. | No fallback needed. | — |
| 2026-07-31 | Step 10c reviewer — RNA neighbors/impact → bounded `grep` | minor | Reviewer questions "who reads `recovery_source_job_id`" and "who calls `write_store`" cannot be answered: this worktree has no LSP enrichment, so `Calls`/`ReferencedBy` edges are absent (`lsp_call_references: unavailable`). This is the finding-critical query for dead-field detection. | Two bounded repo-scoped `grep` sweeps proved the field has no reader (finding W2). Two friction events. | Same follow-up as the execute log: syntax-level same-crate reference fallback when LSP edges are unavailable. |
| 2026-07-31 | Step 10c reviewer — RNA search → bounded `grep`/`sed` for static tables and string literals | minor | Verifying reconstruction stability required reading *static data tables and match arms*, not symbols: `infer_language_from_path` arms, `C_CONFIG`/`CPP_CONFIG` `language_name`+`extensions`, `BUILTIN_LSP_DESCRIPTORS` influence-pattern literals, `meta_name_col` Arrow write/read sites, `.oh/.cache` path constructions. Symbol search returns the enclosing function but not the literal rows, and ranked search surfaced unrelated markdown instead. | Six bounded `grep`/`sed` sweeps scoped to files RNA had already located. They produced blocking findings B1 and B4. Six friction events. | A "constants/table rows for this symbol" or per-file literal projection would keep identity-stability review inside RNA; today the highest-value reviewer evidence lives in match arms that RNA does not project. |
| 2026-07-31 | Step 10c reviewer — raw `git diff` | n/a | RNA has no diff projection, so the mandated final-diff read used `git diff origin/main...HEAD` plus the pre-generated patch. Not treated as substitutable by RNA. | Four bounded diff reads (per-file). | Consider a commit-range projection so review agents can read a diff with RNA provenance attached. |

| 2026-08-01 | Step 10c reviewer (round 3) — RNA MCP `search` with `file`+`line`/`end_line` | none | Bounded current-filesystem retrieval answered every "show me the exact contract" question for the round-3 review: `begin` disposition state machine (`work_items.rs:420-600`), `LspWorkItemRecord`/`LspWorkItemStore` shape, `load_store`/`write_store`, `source_snapshot_identity` git-status block, `select_work_items_for_report`, `Node`/`Edge`, `node_lsp_position` call site, `LspPass1WorkItem` seed construction, `LspWorkItemQueueSnapshot::render`. | Ten bounded spans replaced ten `Read` calls. Zero friction. | — |
| 2026-08-01 | Step 10c reviewer (round 3) — RNA CLI `search --compact` | none | Worktree binary confirmed the live index (52,508 symbols) and resolved `LspWorkIdentity`, `NAME_COL`/`name_col` producers, `Edge`. | No fallback needed. | — |
| 2026-08-01 | Step 10c reviewer (round 3) — RNA neighbors/impact → bounded `grep` | minor | Three finding-critical reader/consumer questions were unanswerable because this worktree has no LSP enrichment (`lsp_call_references: unavailable`, no `Calls`/`ReferencedBy` edges): "who reads `recovery_source_job_id`" (produced blocking finding B2 — the field has no reader), "who consumes `recovery_dispositions`/`LspWorkItemQueueSnapshot::render`" (delivery proof for `computed-but-not-delivered`), and "who calls `flush`/`maybe_flush`". | Four bounded repo-scoped `grep` sweeps. Four friction events. | Unchanged and now load-bearing four sessions running: syntax-level same-crate reference fallback when LSP edges are absent. Dead-field detection is the single highest-value reviewer query and RNA cannot answer it in a fix worktree. |
| 2026-08-01 | Step 10c reviewer (round 3) — RNA search → bounded `grep` for static tables and literals | minor | Judging the new `git` CLI dependency and the influence-pattern breadth required reading literal rows, not symbols: `Command::new("git")` production vs test call sites, `BUILTIN_LSP_DESCRIPTORS` `partition_influence_patterns:` literals, `DEFAULT_EXCLUDES`, `join(".oh")` path constructions, `FLUSH_INTERVAL`. Ranked search returned unrelated markdown for these shapes. | Five bounded sweeps scoped to files RNA had already located. Five friction events. They produced blocking finding B3 and warnings W6/W8. | Same as rounds 1-2: a per-file literal/table projection would keep identity-and-cost review inside RNA. |
| 2026-08-01 | Step 10c reviewer (round 3) — raw `git diff` / `gh` / workflow YAML | n/a | RNA has no diff projection, no CI-run projection, and does not index GitHub Actions job `if:` conditions. The mandated final-diff read, the reviewed-SHA-vs-HEAD check (HEAD had already advanced to `67de388b`), and the `smoke`-job gating check (`workflow_dispatch`/tag only, so the changed `mcp-smoke.mjs` assertions never run on a PR) all required raw inspection. | Six diff/`gh`/YAML reads. Not treated as RNA-substitutable. | Consider a commit-range projection and a CI-status projection so review agents can gather release evidence with RNA provenance attached. |
| 2026-08-01 | Ship agent, finding remediation — RNA search → bounded `grep`/`sed` | minor | Verifying the round-2 blocking findings needed the same two unsupported query shapes: static table rows (`C_CONFIG.language_name`/`extensions` vs `infer_language_from_path` match arms — the B1 evidence) and call-site enumeration of private functions (`record_integrity_digest`, `write_store`, `merge_and_write_store`, `select_work_items_for_report`, `current_request_anchor`). Dead-code confirmation after removing the report-path wrappers also required a repo-wide textual sweep, because with no `Calls` edges RNA cannot answer "is this now unreferenced". | Nine bounded sweeps, all scoped to files RNA had already located. Nine friction events. | Unchanged, and now load-bearing three sessions running: syntax-level same-crate references, plus a per-file literal/table projection. |

### Friction totals

| Session segment | Events | Dominant cause |
|---|---|---|
| Ship agent — initial audit of `f97bdb38` | 8 | 2 caller/impact gaps, 6 in-file mention sweeps |
| Step 10c reviewer (round 2) | 8 | 2 caller/impact gaps, 6 static-table literal sweeps |
| Ship agent — round-2 finding remediation | 9 | 4 caller/impact gaps, 5 static-table sweeps |
| Step 10c reviewer (round 3) | 9 | 4 caller/impact gaps, 5 static-table/literal sweeps |
| Ship agent — round-3 finding remediation | 6 | 3 caller/impact gaps, 3 static-table sweeps |
| **Total** | **40** | **15 caller/impact gaps, 25 literal/mention sweeps** |

Round-3 remediation detail: confirming the dead `recovery_source_job_id` field
(blocking B2) needed a repo-wide reader sweep that RNA cannot answer without
`ReferencedBy` edges; enumerating `update_path_identity` and `MAX_ATTEMPTS` call
sites needed the same shape; and judging the exclusion fix required reading the
`DEFAULT_EXCLUDES` literal rows in `scanner.rs`, which symbol search returns only
as an enclosing const. Reading that table directly is what caught that `vendor/`
and `.cache/` are already excluded — which corrected a wrong assertion in the
first draft of the regression test.

`git diff` reads are excluded from the totals: RNA has no diff projection, so
they are not an RNA substitution.

**Recurring theme:** short-lived fix worktrees never have LSP enrichment
attached, so every ship/review pass in a worktree loses caller/reference
navigation and falls back to bounded text search for exactly the questions a
reviewer asks most. Two query shapes account for all 25 events: "who references
this symbol" and "which literal rows belong to this table". Both blocking
findings that the round-2 review contributed (B1, B4) came out of the second
shape, and confirming the round-2 fixes required both — so this is not
incidental friction, it is the review workload itself falling outside RNA.
