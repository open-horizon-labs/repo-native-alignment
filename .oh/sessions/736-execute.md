---
title: Issue #736 - Resolve or time-bound the remaining paste advisory
date: 2026-07-15
issue: 736
---

# Issue #736 — Resolve or time-bound the remaining paste advisory

## Aim

Keep the all-target RustSec gate truthful after the Lance and Metal parent
migrations: remove `paste` if a compatible owning-parent move exists, otherwise
make the residual maintenance risk explicit, narrow, testable, and temporary.

## Residual graph after #734 and #735

`cargo +1.97.0 tree --locked --all-features --target all -i
paste@1.0.15` resolves one exact package version with 22 direct dependents.

The default graph has ten direct dependents:

- DataFusion 53.1.0: `datafusion-common`, `datafusion-expr`,
  `datafusion-expr-common`, `datafusion-functions-aggregate`,
  `datafusion-functions-nested`, `datafusion-functions-table`,
  `datafusion-functions-window`, and `datafusion-physical-expr`;
- Lance 7.0.0: `lance-bitpacking` and `random_word` through
  `lance-datagen`.

The `embeddings` graph adds twelve direct dependents, and `metal` selects the
same package set with additional GPU features:

- GEMM 0.19.0: `gemm`, `gemm-c32`, `gemm-c64`, `gemm-common`, `gemm-f16`,
  `gemm-f32`, and `gemm-f64` through Candle 0.9.2;
- tokenizer/image paths: `macro_rules_attribute`, `pulp`, `rav1e`,
  `tokenizers` 0.20.4 through metal-candle, and `tokenizers` 0.22.2 through
  FastEmbed.

## Solution space

### A. Move the owning parents to compatible published versions

Preferred if any published stack removes every path.

Rejected after isolated current-version probes:

- LanceDB 0.31.0 / Lance 8.0.0 still selects `paste` through DataFusion 53.1
  and `lance-bitpacking`; #734 also found two live RustSec vulnerabilities in
  that all-target graph.
- DataFusion 54.0.0 removes several direct uses but still selects `paste`
  through Parquet 58.3.0.
- FastEmbed 5.17.3 still selects it through tokenizers 0.22.2 and the image
  stack.
- Candle 0.11.0 still selects it through GEMM 0.19.0 and tokenizers 0.22.2.

These probes include the smallest patch move and the available larger parent
moves. None can make the all-feature graph clean.

### B. Alias or patch `paste` to a maintained fork at the leaf

Rejected. Transitive parents depend on the `paste` package by name; replacing
it with `pastey` requires changing the owning parent manifests or maintaining a
same-name fork. A source-only substitution would hide the advisory without
retiring the multi-parent maintenance obligation.

### C. Fork every residual owning parent

Rejected by the stop trigger. The default path spans DataFusion, Lance, and
Parquet while optional paths span Candle, GEMM, tokenizers, image, and
FastEmbed. Coordinated forks or replacing those product stacks is a redesign,
not a bounded advisory migration.

### D. Keep one exact, expiring warning decision

Selected. RustSec classifies RUSTSEC-2024-0436 as informational/unmaintained
and publishes no patched `paste` version. `paste` is a compile-time proc macro;
the evidence shows maintenance/supply-chain exposure, not a reported runtime
vulnerability. Keep only version 1.0.15 declared, enumerate feature
reachability and exact direct dependents, record upstream and impact evidence,
and make CI reject missing context, new versions, new advisory IDs, stale
decisions, or undeclared warnings.

The user explicitly directed execution of the complete P1 batch, including
this issue and its accepted time-bounded-exception trade-off. That supplies the
required human judgment; automation supplies and checks the graph evidence.

## Implementation plan

1. Extend the warning-policy contract so feature reachability, upstream status,
   and impact rationale are required and validated.
2. Replace the provisional `paste` summary with the recomputed default,
   embeddings, and Metal paths plus the exact 22 direct dependents.
3. Preserve the existing owner, #736 decision link, review triggers, explicit
   approval evidence, and 2026-09-30 expiry.
4. Add deterministic negative fixtures for missing exception evidence and run
   the live all-target audit.
5. Verify default, embeddings, and Metal builds on Rust 1.91 and 1.97, then run
   the exact-head release, CLI, and MCP delivery gates.

## Acceptance evidence

- [x] Recomputed the post-#734/#735 all-feature reverse tree.
- [x] Probed every currently available bounded parent migration.
- [ ] CI requires feature, upstream, impact, ownership, trigger, and expiry
  evidence for warning decisions.
- [ ] The live all-target audit matches only the exact `paste` decision.
- [ ] Default, embeddings, and Metal verification passes at both Rust
  boundaries.
- [ ] Exact-head release artifact passes CLI and real MCP delivery checks.

## Stop / pivot trigger

Return to solution-space if a compatible published parent graph removes the
last path, or if an upstream/security change turns this informational
maintenance warning into a vulnerability.
