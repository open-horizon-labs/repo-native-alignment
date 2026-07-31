---
issue: 833
outcome: context-assembly
phase: execute
---

# Issue 833 execution handoff

## Aim

Interrupted or incremental LSP recovery must reuse only semantically identical work and remain stable when an identical source tree is reconstructed into a graph again.

## Diagnosis

The retained SymPy analysis established that all 26,303 node-ID plus operation identities were stable while 9,016 legacy `node_input_hash` values changed. The old hash mixed two kinds of state:

- stable/request-relevant fields: stable node ID, language, start line, and requested operations outside the hash;
- reconstructed representation fields: end line, signature, and body.

It was also too narrow: it could not detect a different cross-file source/config snapshot or LSP planner/server toolchain when the target node itself stayed byte-identical. The raw benchmark evidence was intentionally deleted after analysis because the traces and indexes are reproducible; the regression now encodes the real drift shape directly.

## Selected implementation

- Replace the node-materialization hash with a versioned work identity containing the repository source/config snapshot, root-relative source-derived LSP request position, canonical operation digest, and planner/server toolchain contract.
- Use the same source-derived UTF-16 request position for the actual LSP call and for recovery identity.
- Hash the resolved server executable plus arguments, initialization settings, descriptor/query policy, compile overrides, and normalized startup root.
- Seal every persisted record with an integrity digest and retain its source-job lineage.
- Report one deterministic disposition per current candidate: exact carry, component-specific rerun, schema rerun, tamper rejection, or duplicate rejection.
- Recompute and verify the identity against the current source snapshot before recovered work can enter a completeness report.

Schema 5 is a rebuild boundary. Older records are retained only long enough to produce `rerun_schema`; their result evidence is cleared and is never migrated or rebound.

## Regression coverage

- Identical source with changed end line, signature, body, and metadata carries completed work.
- Source/config or cross-file content changes rerun with `rerun_source_snapshot`.
- Different commits with the exact same Git tree retain the same source identity; commit metadata alone never forces LSP replay.
- Request-anchor, operation, toolchain, and schema changes rerun with their specific dispositions.
- Tampered result records and duplicate retained identities, including the same identity retained by two interrupted jobs, fail closed.
- An unborn Git repository falls back to a deterministic content snapshot instead of making Git a hard requirement.
- Actual LSP request columns come from current source and use UTF-16 units rather than derived `name_col` or signature text.
- Fresh-reopen completeness accepts exact inherited evidence and rejects stale or self-inconsistent work identities.

## Verification so far

- `cargo check --locked --lib`
- `cargo clippy --locked --lib -- -D warnings`
- LSP work-item recovery tests: 27 passed.
- LSP completeness/provenance tests: 59 passed.
- Broader LSP tests: 118 passed, 2 ignored for installed-server requirements.
- Structural-cache replay tests: 4 passed.
- Full library suite: 2,386 passed, 4 ignored.
- Default all-target suite: library, binary, CLI-contract, cache-chain, integration, and doctest groups all passed; installed-server-only groups remained explicitly ignored.

CI, real-client/manual delivery verification, and independent final-diff approval remain ship gates.
