# Issue 733 execution handoff

## Aim

Interrupted LSP work resumes from durable item state, preserving completed progress and making exhausted work visible and actionable.

## Problem

#730 persists Pass 1 identity and state, but a later run still creates a fresh queue. Replaying the full repo wastes completed work; failing every non-terminal item throws away safe partial progress.

## Chosen solution

Add recovery policy at the existing `LspWorkItemLedger::begin` seam. Match persisted items by stable node identity plus requested operations, carry terminal completed/skipped records forward without scheduling them, re-enqueue pending or stale in-flight records with an incremented attempt, and mark records exhausted once the bounded attempt policy is reached. Keep graph application idempotent through the existing edge identity/deduplication path.

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
