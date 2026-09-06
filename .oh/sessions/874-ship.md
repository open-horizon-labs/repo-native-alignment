## Ship Pipeline — PR #874
**Started:** 2026-09-06T16:47Z
**Issue:** #873 — LSP Pass 1 quadratic work / body clones -> OOM
**Branch:** 873-lsp-pass1-symbol-index (worktree .claude/worktrees/873)

### Pre-flight
- AGENTS.md read; guardrail computed-but-not-delivered read (metis file by that name does not exist; guardrail does)
- CodeRabbit: draft PR, not reviewed yet (1 auto comment)
- RNA scan gate: worktree rescanned incrementally, 52392 symbols, Pass1SymbolIndex indexed

### Step 1: RNA-Grounded Review
**Verdict:** CONTINUE
**Metis checked:** 4 (operation-aware-lsp-query-admission, changed-file-lsp-scope, broad-lsp-references, degraded-enrichment) — all honored
**Guardrails checked:** 5 — none violated; computed-but-not-delivered N/A
**Findings:** 6 (1 AC test gap: ambiguous resolution untested; double body-free storage; unused Default derive; plain sub vs saturating_sub; too_many_arguments allow; no memory measurement)
**Callers:** all consumers of changed symbols are inside passes.rs/mod.rs and updated

### Step 2: Independent Code Review
**Status:** code-reviewer agent spawned (diff + AC + guardrails + metis + graph impact; no session context); awaiting verdict

### Prep for 7a
- Baseline: origin/main 84f9270 detached worktree in scratchpad, own target dir (release build in flight)
- Branch: release build in worktree target (in flight)
- Measurement: `/usr/bin/time -l` on `scan --full` then `enrich --capability call-references --scope repo --no-background-continuation`; edge-table diff via pylance (venv ready)
- Ignored integration test available: `cargo test --lib -- --ignored test_lsp_enrichment_produces_edges`

### Step 2: Independent Code Review — result
**Verdict:** COMMENT (no blocking findings)
- warning: ambiguous unique_function case untested (AC6)
- warning: tie-break test re-implements enrich_trait_node filter chain
- warning: residual double body-free storage incl. signature/metadata in graph_by_file
- nit: unchecked self.nodes[index]; nit: unused Default derive
- Equivalence claim (AC7) holds on inspection; ledger identity body-independent (work_items.rs:3721 test)

### Step 3: Fix
Applied to passes.rs (uncommitted until tests pass):
- `implementor_at(file, line)` on Pass1SymbolIndex (saturating_sub); enrich_trait_node + tie-break test use it
- `endpoint_clone` for graph_by_file (id/span/language/source only); doc comment states storage trade-off
- `node(index)` legible panic + should_panic test
- ambiguity test `pass1_symbol_index_ambiguous_call_target_falls_back_to_enclosing_symbol`
- dropped unused `Default` derive
- too_many_arguments allow: kept (private diagnostics helper, acknowledged in step 1)
- Finding while writing the ambiguity test: the dev-session "subtlety 4" (duplicate NodeIds -> ambiguous) describes the pre-#800 per-item map. At HEAD `EndpointLookupIndex::build` sorts+dedups by stable id, so identical NodeIds (same file/name/kind; ID has no span) collapse to ONE unique target; only distinct ids sharing (file,name) (e.g. different roots) are ambiguous. Test `pass1_symbol_index_call_target_resolution_unique_vs_ambiguous` encodes both. Unchanged by this PR; candidate metis about verifying plan-stage behavioural notes against HEAD.
- `cargo test --lib extract::lsp`: 202 passed, 0 failed, 2 ignored
- clippy (CI variant) clean; committed 9634ac9, pushed
- Step 3b: PR marked ready
### Step 4: Regression Oracle — posted (3 new/strengthened tests, 202 LSP tests pass)
### Step 7a: measurement started (baseline main first, then branch, sequential)
### Step 8: README — skipped: no new capability, flag, or behaviour change (internal memory/CPU refactor; README has no Pass 1 internals to update)

### Step 7a: Manual verification — posted
Warm like-for-like: scan 101s->99s, Pass 1 58.6s->56.5s, RNA RSS 1820->1706 MiB, LSP edge multiset identical (51,284). Cold/semi-warm runs discarded (rust-analyzer warm-up order). `time -l` RSS = rust-analyzer; sampled RNA RSS via ps.
### Step 5: Merit — MERGE (posted)
### Step 7b: N/A (posted). Step 6: posted. Step 8: skipped (no user-facing change).
### CodeRabbit: 3 findings on the metis file, fixed in f7569fb, replies posted.

### Step 9: Smoke test — full `cargo test`: 2546 passed, 0 failed, 18 ignored (12 suites). No src/smoke.rs in repo.
### Step 10: CI — lint/test pending on f7569fb (all other checks pass; CodeRabbit check pass)
### Final review — fresh code-reviewer spawned on final diff f7569fb (guardrail independent-final-review-for-prs)

### Artifacts
- Ship session: .oh/sessions/874-ship.md (this file); friction log: .oh/friction-logs/874-ship.md (8 entries: 1 failed MCP tool access, 4 fallbacks, 3 skipped)
- Metis recorded: .oh/metis/reverify-plan-semantics-against-head.md
