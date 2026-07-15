# Issue #734 — Remove the lru advisory through the Lance parent chain

## Aim

Eliminate `RUSTSEC-2026-0002` from RNA's base dependency graph without
weakening the RustSec gate, raising the MSRV, or losing local persistence and
search behavior.

## Problem statement

`lru 0.12.5` is selected by Tantivy 0.24.2 through Lance 2.0.0 and LanceDB
0.26.2. RNA does not depend on `lru` directly, so a direct override or lockfile
refresh cannot cross the parent requirements. The repair must migrate the
owning storage/search stack and prove the behavior at RNA's LanceDB boundary.

## Before path

`lru 0.12.5 -> tantivy 0.24.2 -> lance 2.0.0 / lance-index 2.0.0 -> lancedb 0.26.2 -> repo-native-alignment`

## Solution space

### A. Add a direct lru dependency or patch override

This does not change Tantivy's incompatible requirement and risks duplicate
versions while leaving the advisory-bearing path present. Rejected.

### B. Keep the current stack and retain the time-bounded policy record

The #731 gate can represent this state, but a compatible parent migration now
exists. Keeping the exception would spend risk budget without a constraint that
forces it. Rejected.

### C. Upgrade the coherent LanceDB/Lance/Arrow stack

Move to LanceDB 0.31.0, Lance/lance-index 8.0.0, and Arrow 58. Registry metadata
declares Rust 1.91 for both LanceDB and lance-index. Lance 8's index dependencies
no longer list Tantivy, removing the owning `lru` path. Use compiler-driven API
migration and verify RNA's persistence, schema migration, vector/FTS, metadata,
and graph round-trip boundaries. Selected.

### D. Replace LanceDB

This would redesign RNA's local read model to remove one transitive advisory,
with much larger compatibility and performance risk. Rejected.

## Selected plan

1. Update the four coherent direct constraints: `lancedb`, `lance-index`,
   `arrow-array`, and `arrow-schema`.
2. Let `cargo check --lib --no-default-features` enumerate API changes; keep
   adaptations at the existing LanceDB boundary.
3. Remove the exact lru warning record and fixture finding from the #731 policy.
4. Prove no `lru@0.12.5` (or duplicate advisory-bearing version) remains.
5. Run no-default, embeddings, persistence/search tests, MSRV check, live
   RustSec, and old-cache/new-binary compatibility verification.

## Acceptance evidence

- [ ] `cargo tree --all-features -i lru@0.12.5` finds no package.
- [ ] RustSec passes without an lru policy record or advisory ignore.
- [ ] LanceDB graph load/save, incremental persistence, schema migration,
  custom metadata, FTS, and vector search tests pass.
- [ ] Rust 1.97 no-default and embeddings checks pass.
- [ ] Rust 1.91 remains the declared and verified MSRV.
- [ ] A new exact-head artifact reads cache data written by the pre-upgrade
  exact-head artifact.

## Stop / pivot triggers

- Stop if Lance 8 cannot read or deliberately rebuild RNA's existing cache.
- Return to solution space if the migration requires raising Rust above 1.91 or
  replacing the persistence model rather than adapting bounded APIs.
