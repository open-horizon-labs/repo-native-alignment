---
issue: https://github.com/open-horizon-labs/repo-native-alignment/issues/733
outcome: context-assembly
phase: execute
status: complete
updated: 2026-07-15
---

# Issue 733 execution handoff

## Aim

Interrupted LSP work resumes from durable item state, preserving completed progress and making exhausted work visible and actionable.

## Problem

Issue `#730` persists Pass 1 identity and state, but a later run still creates a fresh queue. Replaying the full repo wastes completed work; failing every non-terminal item throws away safe partial progress.

## Chosen solution

Add recovery policy at the existing `LspWorkItemLedger::begin` seam. Match persisted items by stable node identity, source-input fingerprint, and requested operations; carry terminal completed/skipped records forward without scheduling them; re-enqueue pending or stale in-flight records with an incremented attempt; and mark records exhausted once the bounded attempt policy is reached. Keep graph application idempotent through the existing edge identity/deduplication path.

This extends the shared durable work-item control plane. It does not add leases, a second scheduler, or changed-file planning.

## Acceptance checks

- A mixed persisted queue classifies completed, skipped, retryable, and exhausted items deterministically.
- Only retryable/new work reaches Pass 1 workers; completed/skipped/exhausted items are not replayed.
- Retry attempts, prior phase, and recovery error survive reload.
- Retry count is bounded and exhausted work renders an actionable next step.
- Queue and OperationReport surfaces deliver resumed/skipped/retried/exhausted counts.
- Retried graph output remains idempotent.

## Verification plan

Start with `cargo check --lib`, focused interrupt/restart and rendering tests, an adversarial mixed-queue oracle, full `cargo test`, real MCP smoke, and the complete project `/ship` gate.

## Implementation

- Upgraded the work-item schema to persist successful per-item edges and virtual nodes, so completed work can be restored without repeating the LSP request.
- Recovered the latest overlapping unfinished job by stable node identity, source-input fingerprint, and requested operations; legacy schema-v1 records and changed inputs replay conservatively.
- Carried completed output and skipped state forward, retried eligible pending/in-flight/failed work up to three attempts, and marked exhausted items with an actionable bounded diagnostic that fails Pass 1 closed.
- Filtered the executor to runnable item IDs and deduplicated recovered/new graph output by stable identity.
- Delivered resumed, retried, exhausted, and exhaustion-detail counts through the existing queue snapshot, OperationReport, and `list_roots` surfaces.

## Focused verification

- `cargo check --lib`
- `cargo +1.97.0 test --lib extract::lsp::work_items::tests -- --nocapture` (13 passed)
- `cargo +1.97.0 test --lib extract::lsp::passes::tests -- --nocapture` (6 passed)
- `cargo test --lib interrupt_restart_executor_fixture_invokes_only_retryable_items_once` (1 passed)
- `cargo test --lib exhausted_recovery_fails_pass1_closed_without_invocation` (1 passed)
- `cargo test --lib changed_node_input_replays_instead_of_carrying_stale_output` (1 passed)
- `cargo test` after rebasing onto `issue/730` (1934 library tests passed, 2 ignored; CLI contract passed; doc tests passed/ignored as declared)
- `git diff --check`

`cargo clippy --lib --tests -- -D warnings` reached the crate but the local Rust 1.97 toolchain reported 78 pre-existing all-target warnings outside this change; neither changed LSP file appeared in that warning set. CI remains the authoritative pinned-toolchain lint gate.

## Review fixes

Independent review caught four recovery holes before commit: schema-v1 completed records had no durable output, exhausted work could report false-green readiness, the restart test stopped above the executor scheduling seam, and recovery ignored source changes. Schema-aware input fingerprints, fail-closed exhaustion, and production scheduling fixtures now cover those paths. Atomic cross-process writes remain safe; distributed recovery claims remain outside this single-agent control-plane scope.
