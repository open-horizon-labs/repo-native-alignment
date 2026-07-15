---
title: Issue #735 - Remove number_prefix from the Metal feature chain
date: 2026-07-15
issue: 735
---

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
3. Verify the fork's embeddings paths on Apple Silicon and make its actual
   resolved MSRV explicit.
4. Point RNA's git dependency at the reviewed fork commit.
5. Remove the exact `number_prefix` policy and fixtures, then prove the package
   is absent from the all-feature/all-target graph.
6. Verify RNA no-default, default, embeddings, and Metal feature boundaries at
   Rust 1.97 and Rust 1.91, followed by exact-head CI and Metal integration.

## Acceptance evidence

- [x] `cargo tree --all-features --target all -i number_prefix@0.4.0` finds no
  package.
- [x] Default and no-default graphs do not gain the optional Metal stack.
- [x] Rust 1.97 no-default, default, embeddings, and Metal checks pass.
- [x] Rust 1.91 remains the verified MSRV.
- [x] The real Metal embeddings integration passes on Apple Silicon.
- [x] RustSec passes without a number_prefix policy record or ignore.

## Implementation evidence

- metal-candle PR #6 merged as
  `9966033a92befe0d760813e3a61152271ecd2822`; RNA pins that reviewed commit.
- The fork's fresh dependency graph made its stale Rust 1.75 declaration
  explicit as Rust 1.88. At that floor, all-feature check and 457 executable
  tests passed, with 9 documentation tests ignored.
- The release-mode Metal embeddings example produced `[3, 384]` on CPU and
  Metal with a maximum output difference of `0.000000`.
- RNA's lockfile removed hf-hub 0.4.3, indicatif 0.17.11, number_prefix 0.4.0,
  ureq 2.12.1, and their obsolete platform packages; metal-candle now shares
  hf-hub 0.5 with fastembed.
- The live RustSec gate reports 778 locked packages, zero vulnerabilities, and
  only #736's declared `paste` warning.
- Rust 1.91 and Rust 1.97 each pass no-default, default, embeddings, and Metal
  library checks with the pinned fork commit.
- RNA's ignored product-level rerank integration passed with Rust 1.97 and the
  full embeddings feature graph: 1 passed, 0 failed, 2,002 filtered out.

## Stop / pivot triggers

- Stop if hf-hub 0.5 breaks the real Metal inference path or requires a Rust
  version above 1.91.
- Return to solution space if the fork migration expands beyond its hf-hub
  boundary into a Candle/model redesign.
