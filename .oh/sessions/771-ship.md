# Ship Pipeline — PR #771

**Started:** 2026-07-15
**Issue:** #765 — Unify LSP enricher construction and exclude synthetic nodes

## Pre-flight

- Draft PR #771 is linked to #765 and targets `main` from `issue/765`.
- Exact RNA graph is available with 23,623 cached symbols.
- Fresh full-scan LSP enrichment degraded on rust-analyzer `didOpen` writes; exact diff review continues with that limitation explicit.
- CodeRabbit review is pending the mandatory ready-for-review transition.

## Step 1: RNA-Grounded Review

**Verdict:** CONTINUE
**Metis / situated context checked:** 2 entries
**Guardrails checked:** 5
**Findings:** 4 non-blocking concerns, each resolved by contract evidence or explicit issue scope.

## Step 2: Independent Code Review

**Verdict:** REQUEST CHANGES

Blocking finding: built-in profiles without a LangConfig kind allow-list still admitted declared `Const` nodes to Pass 1 reference work, violating #765's accepted default-off trade-off and bypassing #768's measurement gate.

## Step 3: Fix

**Status:** Complete

- Added a shared `declared_const_references` eligibility-policy flag to the built-in descriptor and defaulted every built-in profile to `false`.
- Applied that policy through the single descriptor factory used by both construction paths.
- Added a Pass 1 admission boundary that rejects declared `Const` nodes unless a future measured profile explicitly opts in, while preserving declared constants for file-level passes and rejecting synthetic nodes globally.
- Added regression assertions for Python factory policy, EventBus/registry parity, synthetic constants, and declared-constant Pass 1 exclusion.
- Verification after the fix: `cargo check --lib` clean; `cargo test --lib` clean (1,998 passed, 0 failed, 3 ignored); `git diff --check` clean.
