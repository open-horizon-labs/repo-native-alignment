---
pr: 772
issue: 766
outcome: context-assembly
status: implementation-verified
---

# PR #772 ship session

## Aim

An LSP no-progress abort must preserve partial graph output, finish the event-bus finalization contract, and remain visibly degraded through CLI and MCP readiness.

## Review findings

- Returning an error from `LspEnricher` discarded partial output and prevented `EnrichmentComplete`, `AllEnrichmentsDone`, and `PassesComplete`.
- A degraded run must not write `lsp_completed.json`; that sentinel promises complete coverage and suppresses retries.
- Degraded state must be durable in the enrichment job ledger or a restart can reinterpret persisted partial LSP edges as ready.

## Verification

- Focused abort/finalizer, live readiness, and restart-ledger regressions pass.
- `cargo check --lib` passes.
- `cargo check --all-targets` passes.
- `cargo test --all-targets` passes: 2003 library tests passed, 3 ignored, plus integration contracts.
- Independent review findings were addressed in two rounds.
- Exact CI artifact run `29470612974` exposed a real delivery regression: the full foreground pipeline ran LSP once during its nominal extract phase and again during its owned enrichment phase, allowing an empty second pass to overwrite an observed degraded abort as stale/successful. The follow-up fix makes Phase 2 the sole foreground LSP owner and makes the CLI exit non-zero after persisting/rendering a degraded report.
- Final-head CI-artifact MCP verification and CodeRabbit gates remain pending.
