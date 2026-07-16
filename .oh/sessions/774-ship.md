---
session: 774-ship
artifact_type: ship
updated: 2026-07-16
---

# Ship Pipeline — PR #774

## Pre-flight

- PR: #774, branch `issue/768`, closes #768.
- Required RNA scan completed with exact/artifact search ready and degraded
  repo-wide TypeScript LSP coverage.
- Relevant metis: operation-aware LSP query admission and declared-constant
  LSP yield.
- Relevant guardrails: repo-native, no-language-conditionals-in-generic,
  computed-but-not-delivered, test-with-real-mcp-client, and
  no-parallel-cargo-agents.
- No pre-existing CodeRabbit comments existed while the PR was draft.

## Step 1: RNA-Grounded Review

**Verdict:** CONTINUE

RNA reported 53 dependents across six subsystems within three hops of the
shared descriptor. Registry/EventBus parity and changed-file planning have
focused regression coverage. All six issue acceptance criteria are met by the
maintained two-server probe, thresholded decision, metis, and Rust-only
descriptor opt-in.

Findings:

1. Measurement configuration parity was fixed by applying Pyright's built-in
   initialization settings and config hint before the final probe.
2. Changed-file planner/runtime parity was fixed and covered by a focused
   regression.
3. The real-server probe is intentionally ignored in ordinary CI; normal unit
   tests pin the profile decision, while the documented ship command supplies
   real-server evidence.
4. The local full library suite had one environment-sensitive failure because
   current-repo operation history contains the word `aborted`; 2,013 other
   library tests passed. Clean CI is the full-suite authority.

The complete review is posted on PR #774.

## Step 2: Independent Review

**Verdict:** REQUEST CHANGES

The reviewer found two blocking evidence gaps:

1. Rust's profile admits every real `NodeKind::Const`, while the first fixture
   measured only top-level `const`.
2. Independent source/target allow-lists could accept a cross-wired edge.

Both were fixed. The Rust fixture now measures top-level const, static,
static mut, associated const, and an unused control. The correctness oracle
requires the exact sixteen expected file/name edge pairs and exact
multiplicity. The probe also records successful `rust-analyzer --version` and
`pyright --version` output. Two complete expanded-corpus trials pass with Rust
eligible and Pyright ineligible.

The independent reviewer re-reviewed the fixes and approved the PR.

## Step 3: Fix Review Findings

The fixture and correctness-oracle fixes were committed and pushed. Focused
unit tests, `cargo check --lib`, clippy, formatting, and two sequential
real-server probe trials passed.

## Step 3b: Mark Ready

PR #774 was marked ready for review and CodeRabbit review was explicitly
requested.

## Step 4: Regression Oracle

**Verdict:** PASS

The regression oracle confirmed default-deny behavior, Rust-only opt-in,
synthetic-constant exclusion, exact edge correctness, maintained two-server
fixtures, and production-path planner coverage.

## Step 5: Merit Assessment

**Verdict:** MERGE

The change converts a speculative global constant allow-list into measured,
per-server policy. Rust gains useful constant references while Pyright and
unmeasured profiles avoid known timeout/error cost.

## CodeRabbit Review

CodeRabbit requested one documentation correction: the reproducible probe
instructions must run `cargo check --lib` before the ignored real-server test.
The metis and operational LSP documentation now show that required sequential
preflight.
