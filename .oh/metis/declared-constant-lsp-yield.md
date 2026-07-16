---
id: declared-constant-lsp-yield
outcome: context-assembly
title: 'Declared Constant References Must Earn a Per-Server Opt-In'
---

## Decision

Enable declared-constant `textDocument/references` only for the built-in
Rust/rust-analyzer profile. Keep Python/Pyright and every other unmeasured
profile disabled.

This is a per-language/server decision, not evidence for a global `Const`
allow-list. Synthetic constants remain categorically excluded before
scheduling.

## Threshold

A profile clears the opt-in bar only when the maintained probe shows all of:

- at least 80% non-empty responses across five declared constants, including
  one deliberately unused control;
- at least one emitted `ReferencedBy` edge per scheduled constant request;
- 100% correctness in the emitted-edge sample, with no self, literal, unused,
  or unexpected referrer edges;
- zero timeouts and zero request errors; and
- average constant-reference latency no more than 2x the comparable Struct
  reference latency.

The probe compares Function call hierarchy, Trait implementations where
supported, and Struct references so constant yield is not interpreted in
isolation.

## Corpus and method

`tests/fixtures/lsp_const_yield/` contains maintained two-file Rust and Python
projects. Each defines five declared constants: four used from local and
cross-file functions, plus one unused control. The fixtures also contain
functions, a shared `Config` type, and a Rust trait/implementation.

Run the real-server probe sequentially:

```bash
cargo test measure_declared_const_reference_yield -- --ignored --nocapture --test-threads=1
```

The ignored test copies the maintained sources to temporary project roots,
extracts real RNA nodes, opts constants into a test-only enricher, then records
RNA's actual query telemetry and emitted graph edges. Three complete observed
trials produced the same decision.

## Results

| Server / class | Requests | Non-empty | Edges | Aggregate latency | Timeouts | Errors |
|---|---:|---:|---:|---:|---:|---:|
| rust-analyzer / Const, trial 1 | 5 | 4 (80%) | 10 | 5 ms | 0 | 0 |
| rust-analyzer / Const, trial 2 | 5 | 4 (80%) | 10 | 0 ms | 0 | 0 |
| rust-analyzer / Const, trial 3 | 5 | 4 (80%) | 10 | 5 ms | 0 | 0 |
| rust-analyzer / Struct | 1 | 1 | 5 | 0-1 ms | 0 | 0 |
| rust-analyzer / Trait implementations | 1 | 1 | 1 | 0-1 ms | 0 | 0 |
| rust-analyzer / Function call hierarchy | 36 | 12 | 0 | 4-12 ms | 0 | 0 |
| Pyright / Const, trial 1 | 5 | 0 | 0 | 150,005 ms | 5 | 5 |
| Pyright / Const, trial 2 | 5 | 0 | 0 | 150,010 ms | 5 | 5 |
| Pyright / Const, trial 3 | 5 | 0 | 0 | 150,010 ms | 5 | 5 |
| Pyright / Struct | 1 | 0 | 0 | 30,001-30,002 ms | 1 | 1 |
| Pyright / Function call hierarchy | 33 | 13 | 2 | 165-173 ms | 0 | 0 |

Rust's ten constant edges were all compiler-confirmed mappings from the
expected enclosing functions to `RETRY_LIMIT`, `TIMEOUT_MS`, `DEFAULT_PORT`,
or `FEATURE_FLAG`. The deliberately unused `UNUSED_SENTINEL` produced no edge.
The extra two `RETRY_LIMIT` edges came from the expected `make_config` and
`worker_config` functions.

Pyright produced no constant edge to sample. Every constant reference request
hit the 30-second timeout, matching the prior large-corpus warning that broad
Pyright references can consume warm-up time without delivering graph value.

Function call-hierarchy edge counts are fixture-dependent: the Rust fixture's
functions intentionally do not call one another, while the Python constructors
produce two in-fixture calls. These rows are comparison context, not part of
the constant opt-in threshold.

## Consequence

The built-in descriptor is the policy seam. Rust sets
`allow_declared_const_references = true`; the macro default remains false, so
every unmeasured server and Pyright stay denied without generic language
conditionals.

Re-run the maintained probe when a language-server upgrade, transport change,
or fixture expansion could change the decision. Do not infer that one server's
success generalizes to another.

## References

- Issue #768
- PR #774
- `.oh/metis/operation-aware-lsp-query-admission.md`
- `src/extract/lsp/policy.rs`
- `src/extract/lsp/mod.rs`
- `tests/fixtures/lsp_const_yield/`
