---
session: issue-770
artifact_type: session
updated: 2026-07-16
outcomes:
  - context-assembly
---

# Issue #770 — one-instance SWE-bench RNA harness

## Aim

Agents and maintainers can reproduce one SWE-bench Verified task through a
fully enriched RNA instance and a real MCP client, then inspect an auditable
bundle instead of relying on a one-off success story.

## Problem Space

We are optimizing for credible causal evidence about context acquisition.
Hard constraints are an isolated no-upstream-history checkout, real stdio MCP
delivery, official evaluation, complete accounting categories with explicit
unknowns, and no claim that one instance is a benchmark score. The relevant
systems are the Verified dataset, Git snapshot materialization, bounded RNA
enrichment/readiness, arbitrary agent executors, provider telemetry, Docker,
and the official evaluator.

## Problem Statement

Maintainers need a repeatable one-instance evidence harness because the earlier
Django proof cannot be audited or rerun consistently, but current executor,
MCP, enrichment, token, patch, and evaluator evidence is fragmented or absent.

## Solution Space

Considered:

1. A shell script around an executor and evaluator. Rejected because structured
   state, failure preservation, stage accounting, and portable tests become
   fragile.
2. A Rust subcommand. Rejected because this is operational experiment tooling,
   and coupling provider/evaluator churn to the RNA product binary adds an
   unnecessary release surface.
3. A standard-library Python orchestrator with an instrumented stdio proxy and
   an explicit executor report contract. Selected because it keeps the product
   boundary clean while making MCP use, missing telemetry, and failures
   auditable.

## Execute pre-flight

- [x] Aim is clear.
- [x] Constraints and all issue acceptance criteria are loaded.
- [x] RNA index is queryable; degraded TypeScript LSP state is recorded.
- [x] Scope is bounded to scripts, fixtures/tests, docs, and session evidence.
- [x] Draft PR #776 existed before implementation.
- [x] Success requires dry-run tests plus one honest Verified manual run and
      the full ship pipeline.

## Review checkpoint

**Aim:** Produce a reproducible, auditable one-instance run through real RNA
MCP and the official evaluator.

**Status:** Continue.

- Necessary: yes; the prior proof was not rerunnable.
- Aligned: yes; changes are confined to harness tooling, fixture tests,
  documentation, CI coverage, and workflow evidence.
- Sufficient: yes for the implementation increment; the manual Verified run
  and ship gates remain outstanding.
- Mechanism clear: yes; the harness controls checkout materialization,
  readiness, MCP delivery, executor evidence, patch creation, and evaluation.
- Changes complete: no claim yet; manual and ship evidence are still required.

Review findings fixed before the first implementation commit:

1. The exact dataset row may contain the gold and test patches. It is now
   withheld from disk until the executor exits, then passed to the evaluator as
   a pinned local dataset snapshot.
2. MCP orientation accounting now includes only `tools/call` responses observed
   before the first edit, not initialization or post-edit traffic.
3. Structural mode now uses `scan --extract-only`; call-reference modes use
   explicit `--no-embed`, and full mode requests embeddings separately.
4. The stdio proxy originally waited for server EOF before closing server stdin.
   Client EOF now closes the child stdin and the live RNA protocol smoke exits
   cleanly.
5. Provider aggregate totals are not double-counted as stage events, and
   unobservable handoff/verification categories remain explicit unknowns.
6. CI exposed that dry-run validation still required an installed RNA binary
   even though it never launches RNA. Dry run now records binary availability
   and an unknown checksum when absent; live runs still fail closed.

Verification:

- `python3 -m py_compile` for the harness, proxy, and tests.
- `python3 -m unittest scripts/tests/test_swebench_rna_one.py`: 5 passed.
- Live proxy protocol smoke: RNA initialize and tools/list round-trip with
  correlated trace rows and clean shutdown.
- Token-free command-line dry run: isolated one-commit/no-remote checkout,
  exact fixture dataset snapshot, six-stage ledger, prediction, manifest, and
  official evaluator command.
- `git diff --check`: pass.

## Manual run finding — UTF-8 diagnostic panic

Two full `django__django-13279` attempts were preserved before executor start:

- Installed RNA 0.2.10, SHA-256 `bc209139…`.
- Exact-head CI artifact for `c0575b40`, SHA-256 `877ba440…`.

Both extracted roughly 100,977 symbols in 3.2 seconds, ran bounded Pyright
enrichment for more than six minutes, then panicked while forming an LSP
diagnostic node name because `message[..77]` split a non-breaking space. No
model tokens were spent and the gold-bearing dataset snapshot was never
written.

The current fix truncates diagnostic display names by Unicode character while
preserving the full message in metadata. Verification:

- `cargo check --lib --no-default-features`: pass.
- Focused multibyte regression: pass.
- All 19 `build_diagnostic_nodes` tests: pass.
- Scoped Rustfmt check and `git diff --check`: pass.

## Successful Verified run

The exact-head `b0fabc95` CI artifact completed
`django__django-13279` end to end:

- isolated checkout proof: one local commit and no remotes;
- full call/reference scan completed and the readiness probe succeeded;
- embeddings were honestly recorded as degraded because the fast-release
  artifact was built without the `embeddings` feature;
- two real RNA searches occurred before the first edit and delivered 13,973
  response bytes;
- first meaningful edit occurred after 91.269 seconds;
- Claude Sonnet cost USD 0.40624545 and reported 30 uncached input, 28,313
  cache-creation input, 651,309 cache-read input, and 6,929 output tokens;
- 404 focused Django tests passed;
- the official SWE-bench evaluator reported one submitted, one completed, one
  resolved, and zero errors.

The successful bundle is
`/tmp/swebench-rna-770-django-13279-b0fabc95`. The exact dataset row remained
withheld until after executor exit.
