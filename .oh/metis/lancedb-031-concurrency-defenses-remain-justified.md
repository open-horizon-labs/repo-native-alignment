---
id: lancedb-031-concurrency-defenses-remain-justified
outcome: context-assembly
title: LanceDB 0.31 did not justify removing RNA write serialization or retries
source_issue: 746
---

## What was measured

RNA's incremental graph persistence was exercised through genuine child
processes sharing one local LanceDB store. The regression matrix varies two
controls independently:

| Scenario | Process scheduling | Merge retry limit | Attempted writes | Final unique rows | Elapsed |
|---|---:|---:|---:|---:|---:|
| serialized with retries | sequential | 3 | 24 | 25 | 500 ms |
| serialized without retries | sequential | 0 | 24 | 25 | 163 ms |
| concurrent with retries | parallel | 3 | 24 | 25 | 103 ms |
| concurrent without retries | parallel | 0 | 24 | 25 | 91 ms |

The extra row in each result is the baseline row. All stable IDs were unique
and visible after reopening the store. The no-retry cases completing means the
small incremental workload observed no merge conflict; it does not prove that
conflicts cannot occur.

## Decision

Retain both safeguards.

The in-process mutex covers more than incremental `merge_insert`: foreground
full persists, background scanner finalization, schema migration fallback,
root pruning, version-pointer flips, compaction, and embedding writes share the
same storage boundary. The child-process matrix deliberately proves that
incremental writes are currently robust under one reproducible pressure test,
but it cannot make the broader full-persist and embedding races disappear.

The bounded conflict retries remain cheap insurance for separate RNA processes,
which cannot share the Tokio mutex. A zero-conflict sample is insufficient to
retire a defense added after real failures. Removal would require a deterministic
way to force and observe every supported writer overlap, including interrupted
full persists and embedding delete-plus-add operations.

## Reproduction

Run:

```text
cargo test --lib server::store::persist::tests::cross_process_write_matrix_preserves_all_rows -- --nocapture
```

The test uses the current Rust test executable as two independent OS processes,
not Tokio tasks, and validates final persisted rows after every scenario.

## Guardrail

Do not infer concurrency safety from a successful no-retry benchmark alone.
Only remove a persistence defense when the test matrix covers every writer that
the defense coordinates and can deterministically exercise the historical race.
