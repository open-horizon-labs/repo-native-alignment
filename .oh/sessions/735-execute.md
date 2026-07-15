# Issue #735 — Remove number_prefix from the Metal feature chain

## Aim

Remove `RUSTSEC-2025-0119` from RNA's optional Metal dependency graph while
preserving the real Metal rerank capability and keeping the feature optional.

## Problem statement

`number_prefix 0.4.0` is selected by `indicatif 0.17.11` through `hf-hub
0.4.3` in RNA's repo-owned `metal-candle 1.3.0` fork. RNA cannot safely patch
the leaf crate: the owning `indicatif` requirement must move, and the real
Metal inference path must continue to compile and execute.

## Before path

`number_prefix 0.4.0 -> indicatif 0.17.11 -> hf-hub 0.4.3 -> metal-candle 1.3.0 -> repo-native-alignment (feature: metal)`

## Solution space

### A. Skip Metal in audit or remove the feature

This hides the advisory by deleting coverage or capability. It violates the
issue's risk-retirement condition. Rejected.

### B. Force number_prefix or indicatif in RNA

RNA does not own either requirement, and a direct dependency cannot change
`hf-hub`'s incompatible `indicatif 0.17` selection. Rejected.

### C. Upgrade the repo-owned metal-candle fork to hf-hub 0.5

`hf-hub 0.5` moves to `indicatif 0.18`, which uses `unit-prefix` instead of
`number_prefix`. This is the smallest owning-parent migration and avoids the
larger hf-hub 1.0 API jump. Update and verify the fork first, then pin RNA to
the reviewed fork commit and remove the exact RustSec policy/fixture record.
Selected.

### D. Replace metal-candle or the Candle stack

This would redesign the Metal inference boundary to remove one transitive
warning. It has much larger API, model-compatibility, and performance risk.
Rejected.

## Selected plan

1. Open a draft prerequisite PR in `open-horizon-labs/metal-candle` before code.
2. Upgrade only `hf-hub` 0.4 to 0.5 and adapt bounded API changes if the
   compiler identifies any.
3. Verify the fork's embeddings/rerank paths on Apple Silicon and its MSRV.
4. Point RNA's git dependency at the reviewed fork commit.
5. Remove the exact `number_prefix` policy and fixtures, then prove the package
   is absent from the all-feature/all-target graph.
6. Verify RNA no-default, default, embeddings, and Metal feature boundaries at
   Rust 1.97 and Rust 1.91, followed by exact-head CI and Metal integration.

## Acceptance evidence

- [ ] `cargo tree --all-features --target all -i number_prefix@0.4.0` finds no
  package.
- [ ] Default and no-default graphs do not gain the optional Metal stack.
- [ ] Rust 1.97 no-default, default, embeddings, and Metal checks pass.
- [ ] Rust 1.91 remains the verified MSRV.
- [ ] The real Metal rerank integration passes on the macOS runner.
- [ ] RustSec passes without a number_prefix policy record or ignore.

## Stop / pivot triggers

- Stop if hf-hub 0.5 breaks the real Metal inference path or requires a Rust
  version above 1.91.
- Return to solution space if the fork migration expands beyond its hf-hub
  boundary into a Candle/model redesign.
