---
session: 775-ship
artifact_type: ship
updated: 2026-07-16
---

# Ship Pipeline — PR #775

## Pre-flight

- PR: #775, branch `issue/769`, closes #769.
- Dedicated issue agent follows repo-local `oh-task` under
  `dev-pipeline-oversight`; no worktree was created.
- The two unrelated untracked `.oh` files are explicitly excluded.
- RNA CLI index is live and supplied symbol, graph-impact, metis, and
  guardrail context before bounded source inspection.
- Relevant metis: operation-aware LSP query admission, changed-file scope
  scheduler/gate parity, and broad-reference closed-scope/shared-circuit.
- Relevant guardrails: repo-native, computed-but-not-delivered,
  no-language-conditionals-in-generic, test-with-real-mcp-client, and
  no-parallel-cargo-agents.

## Implementation

- Broad struct/enum/type-alias/constant `references` are default-denied.
- Explicit changed-file, target-symbol, and task-relevant declarations resolve
  to exact stable-node-ID sets and cannot widen to a root/repository scan.
- One shared request/time circuit is carried through every language enricher.
- Durable jobs record full/scoped/partial/unavailable evidence with scope,
  declared nodes, limits, consumption, elapsed time, and circuit state.
- Verbose search/list-roots readiness renders the durable evidence.
- Deterministic extraction and normal search remain independent of LSP state.

## Verification

- `cargo check`: pass.
- `cargo test --no-run`: pass.
- Focused policy, scope, persistence, readiness, and LSP tests: pass.
- `cargo clippy --lib -- -D warnings`: pass.
- Manual explicit target-symbol enrichment: stable-ID scope resolved to one
  node, scheduled exactly `1/1` request, stayed within `30000ms`, and persisted
  `scoped` evidence rendered by verbose search.
- The first ready-state CI run exposed five stale legacy assertions that still
  expected default warm-up to mean full broad-reference readiness. The
  assertions now enforce the new `default_profile`/partial contract.
- The next run exposed one binary-level assertion that treated the process-local
  `Complete` sentinel as stronger than persisted partial evidence. The renamed
  regression now proves durable evidence replaces that optimistic sentinel.
- Remaining gates: clean ready-state CI, CodeRabbit approval, exact-head CI
  artifact MCP delivery, final comment sweep, merge, and post-merge audit.

## Independent review

Initial verdict: **REQUEST CHANGES**.

Findings and fixes:

1. A per-operation rejection could consume the shared request counter.
   Admission now reserves the local operation budget first, with a two-profile
   regression proving rejected work does not consume or open the shared
   circuit.
2. The elapsed-time limit was polled only in Pass 1. `LspConsumer` now wraps
   the complete enricher future, including server startup and Pass 0, and
   converts deadline expiry into a degraded completion so the graph still
   finalizes. A slow pre-pass regression proves the boundary.
3. Successful default warm-up was mislabeled `full`. Durable/live readiness
   now uses `default_profile`, reports partial broad-reference coverage, and
   keeps global dead-code blocked. `full` is reserved for genuinely complete
   repository evidence.
4. Service/MCP contract coverage now renders `default_profile`,
   `full`, `scoped`, `partial`, and `unavailable`, including budget/circuit
   fields.

Focused fixes, `cargo check --lib`, and `cargo clippy --lib -- -D warnings`
pass. Independent re-review verdict: **APPROVE**.

## CodeRabbit changes-requested follow-up

The authoritative review on commit `3fe2f0f3` contained three inline findings
and two outside-diff findings. The O(nodes × selectors) scope planner finding
was already resolved on `c681d987`; the remaining four were addressed:

1. Every live changed-file background/incremental completion now publishes
   scoped readiness, matching its durable `EnrichmentScope::ChangedFiles`
   evidence. Repository warm-up alone retains `default_profile`.
2. Degraded repository evidence now hydrates and renders with repo-wide
   coverage plus the persisted edge count; it no longer claims explicit
   scoped review context.
3. Zero-edge `default_profile` completion retains the omitted-broad-reference
   explanation instead of falling through to generic zero-coverage wording.
4. Shared deadlines are enforced inside `LspEnricher`. Initialization, Pass 0,
   and later passes are bounded around the caller-owned result, while Pass 1
   owns both the shared-budget and job watchdog branches so every abort drains
   workers and flushes the durable work-item ledger before returning partial
   output. `LspConsumer` keeps an outer fallback only for enrichers that do not
   declare internal deadline ownership.

Verification after these fixes:

- `cargo check --lib`: pass.
- Eight focused readiness/deadline regressions: pass.
- `cargo clippy --lib -- -D warnings`: pass.
- Targeted rustfmt with child-module traversal disabled: pass.
- `git diff --check`: pass.
- Full library suite excluding the checkout-history-sensitive
  `test_list_roots_from_slugs_lsp_stats_per_language`: 2,031 passed,
  4 ignored, 0 failed. The excluded test independently passes in clean CI;
  locally it reads the real repo's persisted degraded scan text.
- Final independent re-review initially found two P1 gaps (no absolute
  unbudgeted phase deadline and zero fresh scoped coverage). Both were fixed
  with one absolute job deadline plus `max(existing, latest)` scoped coverage.
  Final verdict: **APPROVE**, no blocking findings.

## Superseding CodeRabbit review follow-up

A later CodeRabbit review on the same `3ff1b95e` head superseded the earlier
approval with three additional findings:

1. Initialization cancellation could leave a pre-handshake transport in state,
   causing later calls to mistake a partial initialization for a usable server.
   Initialization now short-circuits only on a pipelined transport, drops stale
   pre-handshake state before retry, and explicitly resets incomplete state
   when initialization errors or the caller-owned deadline interrupts it.
2. Pass 1 error accounting was assigned only after all later passes completed.
   The partial error count is now stored immediately after Pass 1 so a later
   deadline abort increments rather than replaces it.
3. Automatic changed-file paths labeled evidence as scoped while scheduling the
   full primary root. Scanner touched-file tuples now resolve to bounded stable
   node IDs and flow through `lsp_node_filter` in background, graph-update, and
   foreground incremental pipelines. Scoped readiness counts only LSP call
   edges incident to those planned IDs, including valid clean zero-edge runs.

Focused regressions cover pre-handshake cleanup, scanner-path exclusion of
unrelated stable IDs, and scoped call-edge counting.

Verification after the superseding review fixes:

- `cargo check --lib`: pass.
- `cargo clippy --lib -- -D warnings`: pass.
- Three focused new regressions: pass.
- Foreground incremental pipeline regression: pass after excluding empty
  virtual-node paths before changed-file normalization.
- Full library suite excluding the checkout-history-sensitive roots test:
  2,034 passed, 4 ignored, 0 failed.
- Independent re-review, including the empty-path follow-up: **APPROVE**.
