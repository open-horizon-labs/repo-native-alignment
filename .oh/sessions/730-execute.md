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
