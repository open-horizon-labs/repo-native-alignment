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
| 2026-08-01 | Step 10c reviewer (round 4) — RNA MCP `search` with `file`+`line`/`end_line` | none | Bounded current-filesystem retrieval answered every contract question for the round-4 review: `begin` disposition/retry arm (`work_items.rs:495-640`), constants block (`16-40`), `select_recovery_job` (`1596-1665`), `retain_recent_jobs`/`merge_and_write_store` (`1860-1922`), `load_store` (`1930-1990`), `mark_terminal` (`700-800`), `flush`/`maybe_flush` (`800-870`), `select_work_items_for_report` (`lsp_completeness.rs:1596-1717`), `DEFAULT_EXCLUDES` rows (`scanner.rs:37-85`), clangd influence patterns (`mod.rs:440-470`), `startup_root_override` (`mod.rs:1725-1745`, `2278-2300`), `installSyntheticWorkItemLedger` (`mcp-smoke.mjs:20-90`). | Twelve bounded spans replaced twelve `Read` calls. Zero friction. | — |
| 2026-08-01 | Step 10c reviewer (round 4) — RNA CLI `search --compact` | none | Worktree binary confirmed the live index (52,508 symbols) and resolved `LspWorkIdentity` with a stable node ID on the first query. | No fallback needed. | — |
| 2026-08-01 | Step 10c reviewer (round 4) — RNA neighbors/impact → bounded `grep` | minor | Four reader/consumer questions were unanswerable (`lsp_call_references: unavailable`, no `Calls`/`ReferencedBy` edges): "who consumes `recovery_dispositions`/`LspWorkItemQueueSnapshot::render`" (the `computed-but-not-delivered` delivery proof — answered by one grep hit at `server/operation_report.rs:196`), "where is `startup_root_override` set vs read" (needed to judge whether `lsp_toolchain_contract` hashes the venv the server actually gets), "where is `name_col` written and read across the LanceDB round trip" (the load-bearing evidence for warning W3 against `extract-fully-at-parse-time`), and "where is `infer_language_from_path` defined" (the test's import path `src/server/store.rs` does not exist; the module is `src/server/store/mod.rs`). | Four bounded repo-scoped `grep` sweeps. Four friction events. | Unchanged and now load-bearing five sessions running: syntax-level same-crate reference fallback when LSP edges are absent. |
| 2026-08-01 | Step 10c reviewer (round 4) — RNA search → bounded `grep`/`awk` for static tables, literals, and struct field lists | minor | Seven verification questions were table-shaped, not symbol-shaped: `DEFAULT_EXCLUDES` literal rows, `compile_commands.json` in `BUILTIN_LSP_DESCRIPTORS.partition_influence_patterns`, `lsp_enrichable_kinds` rows, `.oh/.cache` path constructions (lance/scan-state/embeddings — needed to prove the snapshot is self-stable), `mcp-smoke.mjs` `in_flight`/`mcp_smoke_probe`/`schema_version` literals, exact line numbers for the finding table, and full field lists for `Node`/`Edge`/`LspWorkItemRecord` (RNA returns `signature_only`, and digest determinism depends on whether any field is a `HashMap`). | Seven bounded sweeps, all scoped to files RNA had already located. Seven friction events. They produced blocking finding **B1** and warnings **W1**/**W3**. | Two asks, both repeated from rounds 1-3: a per-file literal/table projection, and an `include_body`-equivalent for struct field lists so type-shape questions do not need `awk`. |
| 2026-08-01 | Step 10c reviewer (round 4) — RNA ranked search leaked prior-round verdicts | minor | A flat `search("DEFAULT_EXCLUDES", kind=constant)` returned **zero** code hits and instead surfaced two large chunks of this friction log verbatim, including prior rounds' finding labels (B1-B4, W2/W6/W8) and their conclusions. The round-4 reviewer was explicitly instructed to form an independent verdict without reading prior conclusions, so RNA's own markdown-over-code ranking actively worked against the review contract. | Discarded the returned text, used a bounded `grep` for the constant instead, and formed findings from the code. One friction event (counted in the static-table row above, not double-counted here). | Add an artifact-class filter that keeps `.oh/friction-logs/` and `.oh/sessions/` out of `kind`-filtered code searches, or an `exclude_artifact_types` parameter; ranked markdown should never outrank an exact-named `constant` lookup. |
| 2026-08-01 | Step 10c reviewer (round 4) — temporary probe tests + `cargo test --lib` | n/a | Confirming the two blocking findings needed executable evidence, not reading: three throwaway `reviewer_probe_*` tests were added to `work_items.rs`, run with `cargo test --locked --lib reviewer_probe -- --nocapture`, and reverted with `git checkout --` (tree verified clean at `3e7bcba5`). Not an RNA gap — RNA has no execution projection and should not. | Three probe tests, two `cargo test --lib` runs, one revert. | — |
| 2026-08-01 | Step 10c reviewer (round 5) — RNA MCP `search` with `file`+`line`/`end_line` | none | Bounded current-filesystem retrieval answered eighteen contract questions for the round-5 review: `DEFAULT_EXCLUDES` rows (`scanner.rs:37-74`), `is_file_excluded`/`dir_component_matches` (`scanner.rs:1329-1400`), `is_excluded`/`is_excluded_dir` (`1250-1330`), `partition_influence_pattern_matches`+`partition_influence_patterns` (`mod.rs:100-180`), Pass-1 operation dispatch by kind (`passes.rs:1340-1385`), position consumers (`passes.rs:1939-2010`, `2015-2085`), `pass1_operations_for_node` (`mod.rs:930-960`), `with_startup_root`/`set_startup_root` (`mod.rs:1725-1745`, `4145-4175`), enrichable-kind assertions (`mod.rs:5670-5735`), `admits` (`policy.rs:339-400`), `select_work_items_for_report` (`lsp_completeness.rs:1596-1700`), `completed_records` filter + inherited-lineage check (`structural_cache.rs:783-800`, `935-985`), checkpoint replay (`structural_cache_replay.rs:695-730`), `ExtractorRegistry::extract_file` usage (`extract/mod.rs:935-957`), the reconstruction-stability tests (`work_items.rs:3003-3070`), and a markdown line to confirm a real link column (`AGENTS.md:112-114`). | Eighteen bounded spans replaced eighteen `Read` calls. Zero friction. | — |
| 2026-08-01 | Step 10c reviewer (round 5) — RNA CLI `search --compact` | none | Worktree binary confirmed the live index (52,508 symbols) and resolved `LspWorkIdentity`, `DEFAULT_EXCLUDES`, `partition_influence_pattern_matches` with stable node IDs. | No fallback needed. | — |
| 2026-08-01 | Step 10c reviewer (round 5) — RNA MCP `search` over 200-line bound → `Read` | minor | The reviewed diff is 1,986 lines inside a 3,574-line file. Judging a state machine requires reading it *contiguously* — `begin`'s disposition/retry/resume arms span lines 360-654 and cannot be assessed 200 lines at a time without losing the control flow that carries the defect. RNA's bounded retrieval caps at 200 lines and 65,536 bytes per request, so the only way to hold `begin`, `work_identity`, the snapshot functions, and `load_store`/`write_store` in one view was four 560-line `Read` calls. | Four `Read` calls on ranges RNA had already located. Four friction events. | Allow a larger contiguous span (or a `symbol_body` span mode) when the request names a single function/impl block; the 200-line cap is tuned for lookup, not for reviewing a long state machine. |
| 2026-08-01 | Step 10c reviewer (round 5) — RNA neighbors/impact → bounded `grep` | minor | Six reader/consumer questions were unanswerable (`lsp_call_references: unavailable`, no `Calls`/`ReferencedBy` edges), and two of them produced the round-5 blocking findings: "who calls `node_lsp_position`" and "who still reads `name_col`" (together they showed the LSP path is now the *only* reader and it stopped reading — evidence for **B2** and **W1**), "who consumes `produced_result_ids`" and "how is `completed_records` built" (needed to decide whether the non-cleared field is live or latent — **W2**), "where is `startup_root_override` set relative to `lsp_toolchain_contract`" (identity-stability check), and "where is `enrichable_kinds`/`markdown_kind` actually applied" (needed to prove **B2** is live code and not an unreachable arm). | Six bounded repo-scoped `grep` sweeps. Six friction events. | Unchanged and now load-bearing six sessions running: syntax-level same-crate reference fallback when LSP edges are absent. Round 5 repeats round 4's observation — the reader/consumer query is what converts a suspicion into a blocking finding. |
| 2026-08-01 | Step 10c reviewer (round 5) — RNA search → bounded `grep` for static tables and literals | minor | Four verification questions were table-shaped: `DEFAULT_EXCLUDES` consumers (to compare the new `excluded_from_content_snapshot` arms against the scanner's own matcher), `compile_commands.json` inside `BUILTIN_LSP_DESCRIPTORS`, the `partition_influence_patterns:` literal rows across ~40 descriptors, and `lsp_enrichable_kinds` rows in `configs.rs` (only two languages restrict kinds — the fact that decided **B2** is live). | Four bounded sweeps scoped to files RNA had already located. Four friction events. | Same ask as rounds 1-4: a per-file literal/table projection. |
| 2026-08-01 | Step 10c reviewer (round 5) — RNA ranked search missed a plain API lookup | minor | A natural-language `search("extract nodes and edges from a single file entry point", rerank)` returned nine `const` **string literals** (log-message and tool-description strings) and no sign of `ExtractorRegistry::extract_file`, the actual entry point. The one usable hit was a test name, from which the API had to be inferred. Same ranking pathology as round 4's `kind=constant` miss: interned string-literal constants crowd out the function being asked for. | Followed the test name to a bounded span and recovered the API. One friction event. | Down-rank bare string-literal `const` nodes in natural-language code search, or expose a `kind=function` bias for "entry point"/"API" style intents. |
| 2026-08-01 | Step 10c reviewer (round 5) — temporary probe tests + `cargo test --lib` | n/a | Both blocking findings needed executable evidence: four throwaway `probe_*` tests in `work_items.rs` (repeated schema bumps, single schema bump on a retried record, root-level ignored influence file, raw `git ls-files` pathspec shapes) plus one probe that ran the real extractor over 807 repository files and compared `name_col` against `source_request_position`. Run with `cargo test --locked --lib probe_ -- --nocapture`; reverted from a pre-edit copy and the tree verified clean with `git status --porcelain`. Not an RNA gap. | Five probe tests, five `cargo test --lib` runs, one revert. | — |
| 2026-08-01 | Step 10c reviewer (round 6) — RNA MCP `search` with `file`+`line`/`end_line` | none | Bounded current-filesystem retrieval answered every contract question for the round-6 review: `begin`'s disposition/retry/resume arms (`work_items.rs:490-640`), `LspWorkItemRecord`/`LspWorkItemStore` field lists (`120-240`), `select_recovery_job` (`1615-1700`), `load_store`/`write_store` (`1985-2130`), `select_work_items_for_report` (`lsp_completeness.rs:1592-1705`) and its caller `build_and_persist_report_from_evidence` (`1440-1530`), `BuiltinLspDescriptor::partition_influence_patterns`/`build` (`mod.rs:110-160`), `set_startup_root` (`mod.rs:4150-4175`). | Eight bounded spans replaced eight `Read` calls. Zero friction. | — |
| 2026-08-01 | Step 10c reviewer (round 6) — RNA CLI `search --compact` | none | Worktree binary confirmed the live index (52,508 symbols) and resolved `LspWorkIdentity` with a stable node ID on the first query, as the task's verification step specified. | No fallback needed. | — |
| 2026-08-01 | Step 10c reviewer (round 6) — RNA ranked search leaked prior-round verdicts again | major | The *first* substantive query of the round, `search("select_work_items_for_report")`, returned two code hits and then dumped this friction log and `.oh/sessions/833-dev.md` verbatim — including every prior round's finding labels, severities, and conclusions, and the session's own justification for the contract choices under review. The round-6 reviewer was explicitly instructed not to read prior conclusions or `.oh/sessions/833-*`. Round 4 logged the same pathology as `minor` for a `kind=constant` miss; escalating it, because here it defeated the review contract on the opening query with no way to opt out ahead of time. | Discarded the returned prose, re-issued every later query with `include_markdown: false` and `include_artifacts: false`, and formed all findings from code and probe output. One friction event. | Two asks: (1) default `include_markdown`/`include_artifacts` to false when the query is an exact symbol name; (2) add an `exclude_paths`/`exclude_artifact_types` parameter so an independent-review agent can hard-exclude `.oh/sessions/` and `.oh/friction-logs/` for a whole session. Independent review is a first-class RNA workflow and RNA currently cannot be configured to support it. |
| 2026-08-01 | Step 10c reviewer (round 6) — RNA neighbors/impact → bounded `grep` | minor | Five reader/consumer questions were unanswerable (`lsp_call_references: unavailable`, no `Calls`/`ReferencedBy` edges), and two of them decided findings: "who calls `select_work_items_for_report`" (needed to establish that the structural-cache inheritance path reaches it without passing through ledger `begin` — the non-tamper route for warning W1), "who consumes `recovery_dispositions`/`render()`" (the `computed-but-not-delivered` delivery proof, answered by one hit at `operation_report.rs:196`), "who calls `set_startup_root`" (needed to prove the venv/startup-root component of `lsp_toolchain_contract` is set before it is hashed — this cleared a suspected defect), "who writes `name_col`" (the load-bearing evidence for blocking finding B1), and enumerating `Command::new("git")` production call sites. | Five bounded repo-scoped `grep` sweeps. Five friction events. | Unchanged and now load-bearing seven sessions running: syntax-level same-crate reference fallback when LSP edges are absent. Round 6 repeats rounds 4 and 5 exactly — the reader/consumer query is what converts a suspicion into a finding. |
| 2026-08-01 | Step 10c reviewer (round 6) — RNA search → bounded `grep`/`sed` for static tables and literals | minor | Three verification questions were table-shaped: `DEFAULT_EXCLUDES` rows (to check the new extension-vs-suffix arms and the `target*/`, `.cache/`, `vendor/` entries), the ~40 `partition_influence_patterns:` literals (to find a `/`-crossing pattern shape — this produced warning W2), and `ExtractorRegistry::with_builtins` vs `new` (the probe silently extracted zero nodes with `new`). | Three bounded sweeps scoped to files RNA had already located. Three friction events. | Same ask as rounds 1-5: a per-file literal/table projection. |
| 2026-08-01 | Step 10c reviewer (round 6) — temporary probe tests + `cargo test --lib` | n/a | All three findings needed executable evidence: five throwaway `probe_*` tests (Go receiver-named method position, C++ out-of-line constructor position, same-line comment shadowing, nested ignored manifest binding, and a real-extractor run over synthetic Go/C/C++/TypeScript files comparing `name_col` against `source_request_position`) plus one in `lsp_completeness.rs` (report acceptance of an anchor that disagrees with the node's line). Run with `cargo test --locked --lib probe_ -- --nocapture`; reverted from pre-edit copies and the tree verified clean with `git status --porcelain` at `a9594cdd`. Not an RNA gap. | Six probe tests, four `cargo test --lib` runs, one revert. | — |

### Friction totals

| Session segment | Events | Dominant cause |
|---|---|---|
| Ship agent — initial audit of `f97bdb38` | 8 | 2 caller/impact gaps, 6 in-file mention sweeps |
| Step 10c reviewer (round 2) | 8 | 2 caller/impact gaps, 6 static-table literal sweeps |
| Ship agent — round-2 finding remediation | 9 | 4 caller/impact gaps, 5 static-table sweeps |
| Step 10c reviewer (round 3) | 9 | 4 caller/impact gaps, 5 static-table/literal sweeps |
| Ship agent — round-3 finding remediation | 6 | 3 caller/impact gaps, 3 static-table sweeps |
| Step 10c reviewer (round 4) | 11 | 4 caller/impact gaps, 7 static-table/literal/field-list sweeps |
| Ship agent — round-4 finding remediation | 3 | 1 caller/impact gap, 2 static-table sweeps |
| Step 10c reviewer (round 5) | 15 | 6 caller/impact gaps, 4 static-table sweeps, 4 over-200-line span reads, 1 ranked-search miss |
| Ship agent — round-5 finding remediation | 4 | 2 caller/impact gaps, 2 static-table sweeps |
| Step 10c reviewer (round 6) | 9 | 5 caller/impact gaps, 3 static-table sweeps, 1 ranked-search verdict leak |
| Ship agent — round-6 finding remediation | 2 | 1 caller/impact gap, 1 static-table sweep |
| **Total** | **84** | **34 caller/impact gaps, 49 literal/mention/span events, 1 verdict leak** |

**Escalated to `major` by the round-6 reviewer, and it is the most consequential
entry in this log:** RNA's ranked search returned this friction log and
`.oh/sessions/833-dev.md` verbatim on the reviewer's opening query, exposing five
rounds of prior verdicts to an agent explicitly instructed not to read them.
There is no parameter to exclude paths from a ranked search up front. The whole
independent-review guardrail depends on the reviewer not seeing prior
conclusions, and RNA's own default search actively undermines it. Follow-up:
an `exclude_paths` parameter, or a review-mode projection that omits
`.oh/sessions/` and `.oh/friction-logs/` unless explicitly requested. Until then
reviewer prompts cannot rely on instruction alone to keep the review independent.

Round-6 review detail: nine events, and the distribution is now completely
stable across three independent reviewers — 5 caller/consumer gaps, 3
static-table sweeps, 1 ranked-search pathology. The one change is severity: the
verdict leak that round 4 logged as `minor` recurred on the round-6 reviewer's
*opening* query and is logged as `major`, because it exposed every prior round's
findings and the session's own defence of the contracts under review to an agent
instructed not to read them. There is currently no parameter that lets an
independent-review agent exclude `.oh/sessions/` and `.oh/friction-logs/` up
front, so the leak is unavoidable rather than merely likely. Round 6's blocking
finding again came from a probe running the **real extractor over non-Rust
inputs** — a Go method whose name equals its receiver type — confirming for the
third round running that fixture monoculture, not reasoning, is what conceals
defects on this issue.

Round-5 remediation detail: four sweeps. Two were the usual caller/consumer gap
— tracing `load_store`'s invalidation writes against `begin`'s reads to prove
the persisted `recovery_disposition`/`state` pair is overwritten in the same
load (the root cause of three failed fixes), and finding who sets `name_col`.
Two were static-table reads: the `name_col` producers in `markdown.rs`,
`generic.rs` and `java.rs`, and the Arrow write/read rows. Notably, the
root-cause insight came from reading two *writers* and one *reader* of the same
two fields together — a question of the exact shape RNA cannot express without
reference edges, and one that three prior rounds of reasoning-without-tracing
got wrong.

Round-4 remediation detail: three sweeps, all of the shapes already logged.
Confirming the `name_col` persistence claim (round-4 W3) needed the
`meta_name_col` Arrow write/read rows in `graph/store.rs`,
`server/store/batch.rs` and `server/store/load.rs` — a producer/consumer
question RNA cannot answer without `ReferencedBy` edges, and one where being
wrong had already put a false justification into a shipped doc comment. Reading
`DEFAULT_EXCLUDES` again was what showed the `*.o`-matches-`main.go` defect was
mine rather than the reviewer's misreading.

Round-3 remediation detail: confirming the dead `recovery_source_job_id` field
(blocking B2) needed a repo-wide reader sweep that RNA cannot answer without
`ReferencedBy` edges; enumerating `update_path_identity` and `MAX_ATTEMPTS` call
sites needed the same shape; and judging the exclusion fix required reading the
`DEFAULT_EXCLUDES` literal rows in `scanner.rs`, which symbol search returns only
as an enclosing const. Reading that table directly is what caught that `vendor/`
and `.cache/` are already excluded — which corrected a wrong assertion in the
first draft of the regression test.

Round-4 review detail: the same two shapes again. The blocking exclusion-matcher
finding came out of reading the `DEFAULT_EXCLUDES` rows next to the new
`excluded_from_content_snapshot` arms; the retry-budget finding came out of
reading `MAX_ATTEMPTS`, `mark_terminal`, and the `begin` rebuild arm together.
Neither is reachable from a ranked search result. A new shape also appeared:
struct **field lists** (`Node`, `Edge`, `LspWorkItemRecord`) had to be dumped
with `awk` because RNA returns `signature_only`, yet whether the new integrity
digest is deterministic depends entirely on whether any serialized field is a
`HashMap`.

`git diff` reads, `cargo test` probe runs, and `rustc` scratch programs are
excluded from the totals: RNA has no diff or execution projection, so they are
not RNA substitutions.

**Recurring theme:** short-lived fix worktrees never have LSP enrichment
attached, so every ship/review pass in a worktree loses caller/reference
navigation and falls back to bounded text search for exactly the questions a
reviewer asks most. Two query shapes account for all 32 non-caller events: "who
references this symbol" and "which literal rows belong to this table". Every
blocking finding contributed by an independent review round so far has come out
of the second shape — so this is not incidental friction, it is the review
workload itself falling outside RNA.

Round-5 review detail: two new shapes joined the recurring two. First, the
**200-line span cap** became the binding constraint: the round-5 blocking
retry-budget finding lives in the interaction between three arms of one 294-line
function (`begin`), and no 200-line window contains the interaction. Second, the
strongest evidence in this round came from running the **real extractor over 807
repository files** inside a probe test — 105 of 118 markdown link nodes lose
their LSP request column — which is a measurement, not a query, and confirms the
round-4 observation that fixture monoculture is the dominant defect-concealer on
this issue. Both blocking findings were reached through the reader/consumer query
shape that RNA cannot serve in a fix worktree.

**New in round 4:** RNA search actively *harmed* the review contract once. A
`kind=constant` lookup for a Rust constant returned no code and two verbatim
chunks of this friction log, exposing prior rounds' finding labels and verdicts
to a reviewer explicitly told not to read them. Independent review is a
first-class RNA workflow; ranked markdown outranking an exact-named symbol
lookup is a correctness bug for that workflow, not just noise.

## Segment: Ship Step 10c independent final-diff review (round 7)

Reviewer: independent code reviewer, no implementation context. Commit
`c608276940ae2b8bc09f14f61aefc04f06e7954c`. Isolation flags
`include_artifacts: false` + `include_markdown: false` on every RNA search.

| # | Tool wanted | What happened | Fallback used | Cost |
|---|-------------|---------------|---------------|------|
| 1 | `search(query="retain_recent_jobs", include_body=true)` | Rejected: `include_body requires 'node' or 'nodes' parameter`. A flat query cannot ask for a body in one call; it needs a second round trip to resolve the node ID first. | `grep -n "fn retain_recent_jobs" -A 45` | 1 wasted MCP call + 1 grep |
| 2 | Field lists for `Node`, `Edge`, `LspWorkItemRecord` (needed to reason about serde round-trip determinism of the new `integrity_digest`) | `search(kind="struct", compact=true)` returns signature + location only; struct field lists are not retrievable without the node-ID round trip from #1. | `grep -n "pub struct ..." -A N` | 3 greps |
| 3 | Callers of `source_snapshot_identity` / `node_lsp_position` | Known degradation: no LSP enrichment in this worktree, so `Calls`/`ReferencedBy` edges are absent and `mode=neighbors direction=incoming` cannot answer "who calls this". | `grep -n "LspWorkItemLedger::begin" -A 22 src/extract/lsp/passes.rs` | 1 grep |
| 4 | Test-module fixtures (`fn node`, `fn edge`, `fn seeds`) inside `mod tests` | Not surfaced usefully by flat search; private test helpers rank below production symbols. | `grep -n "    fn node(" -A 22` | 2 greps |
| 5 | `.github/scripts/mcp-smoke.mjs` content | `.mjs` is not indexed as a code language, so no symbol or bounded-span retrieval is available. | `sed -n` on the file | 2 reads |
| 6 | Isolation flags did not fully suppress `.oh/` markdown | With `include_artifacts: false` **and** `include_markdown: false`, the query `LspWorkItemLedger::begin` still returned a `markdown_section` hit from `.oh/sessions/733-execute.md` ("Chosen solution", heading only). Unrelated issue, no verdict content, but the suppression contract did not hold. | Disregarded the hit; reported the leak in the PR comment. | Isolation risk |

Bounded-span retrieval (`search` with `file` + `line` + `end_line`) worked well and was
the primary navigation tool: 5 successful spans, no fallback needed.

**Segment totals:** RNA MCP calls 9 (8 successful, 1 rejected on tool contract).
Raw fallbacks 8 grep/sed invocations, each with a diagnosed reason above.
Probe tests: 7 written against the real code paths, 3 passed, 4 failed and became
findings; all reverted, tree left clean apart from this log.
