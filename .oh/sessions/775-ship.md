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
