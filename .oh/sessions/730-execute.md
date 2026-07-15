# Issue 730 execution handoff

## Aim

Make Pass 1 call/reference enrichment explainable and recoverable across process boundaries so agents can trust what completed, what is running, and what failed.

## Problem

The durable job ledger records aggregate state, while `LspPass1WorkItem` and `LspPass1DiagnosticSnapshot` exist only in memory. A restart therefore loses item-level identity, phase, attempts, errors, and queue observability.

## Chosen solution

Extend the existing repo-local LSP control plane with a versioned, atomically persisted work-item ledger. Use one serializable record per bounded queue item, derive snapshots from those records after load, retain a bounded history, and carry the same snapshot through `OperationReport`, `list_roots`, and MCP rendering.

This is persistence and delivery only. Restart/retry policy remains #733; changed-file planning remains #732.

## Acceptance checks

- Mixed pending/in-flight/completed/failed/skipped state round-trips atomically under `.oh/.cache`.
- Older, missing, or malformed persisted state degrades safely.
- Reloaded phase counts and oldest in-flight identity match the pre-restart snapshot.
- Operation reports and `list_roots` render the persisted queue snapshot through MCP.
- Retention cannot grow without bound or alter the executor's existing bounded queue semantics.

## Verification plan

Start with `cargo check --lib`, then focused persistence/rendering tests, mixed-state restart adversarial coverage, full `cargo test`, real MCP smoke, and the complete project `/ship` gate.

## Implementation

- Added a versioned repo-local Pass 1 work-item ledger with atomic temp-file replacement and a bounded two-job history.
- Persisted queue identity, requested operations, lifecycle state, attempts, phases, timestamps, and terminal errors while the existing bounded worker pool executes.
- Reconstructed live queue snapshots from disk and delivered them directly through `list_roots`; completed operation reports retain and link the same job snapshots.
- Documented the control-plane seam in ADR-003 and the user-visible `list_roots` contract in the README.

## Focused verification

- `cargo check --lib`
- `cargo test --lib work_item -- --nocapture`
- `cargo test --lib test_list_roots_from_slugs_includes_live_lsp_work_queue -- --nocapture`
- `cargo test --lib persisted_lsp_work_queue_is_attached_and_rendered_for_list_roots -- --nocapture`
- `git diff --check`

## Review fixes

- Restored the real pipeline start timestamp so completed OperationReports attach queues from the operation they describe, including older terminal snapshots.
- Serialized same-process writers per repository and changed full-store overwrites into merge-by-job writes; a concurrent-ledger regression proves both jobs survive.
- Retained up to 32 active/interrupted jobs and 16 terminal jobs, so polyglot work remains visible while repeated crashes cannot grow the cache without bound.
- Added defaults for legacy non-empty records and regression coverage for aged reports, concurrent ledgers, and bounded interrupted-job history.
