---
id: lancedb-031-concurrency-defenses-remain-justified
outcome: context-assembly
title: LanceDB 0.31 did not justify removing RNA write serialization or retries
source_issue: 746
---

## What was measured

RNA's incremental graph persistence was exercised both with Tokio tasks that
share the same mutex and with genuine child processes sharing one local
LanceDB store. The in-process matrix toggles the actual equivalent of RNA's
serialization boundary independently from its retry limit:

| Scenario | Mutex | Retry limit | Successful mutations | Conflicts | Lock wait | Table version | Final rows |
|---|---:|---:|---:|---:|---:|---:|---:|
| mutex on, retries on | on | 3 | 24 | 0 | 169 ms | 26 | 25 |
| mutex on, retries off | on | 0 | 24 | 0 | 149 ms | 26 | 25 |
| mutex off, retries on | off | 3 | 24 | 0 | 0 | 26 | 25 |
| mutex off, retries off | off | 0 | 24 | 0 | 0 | 26 | 25 |

The separate-process matrix varies process scheduling and retry limit. It does
not claim that scheduling toggles the in-process mutex:

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

The full-persist overlap supplies the decisive serialization evidence. With
the mutex, foreground and background full writers committed scan versions 2
then 3 and the reopened store exposed exactly one complete snapshot. Without
the mutex, both selected scan version 2 and the reopened store exposed the
union of both snapshots (two rows). That violates full-snapshot semantics even
though the store remained readable, so serialization is required.

An interrupted separate-process incremental writer is also killed mid-run and
the store is reopened. The last committed baseline remains visible and stable
IDs remain unique. Typed delete, edge delete, scan-version filtering, and stale
version compaction remain covered by the adjacent persistence regression suite.

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
