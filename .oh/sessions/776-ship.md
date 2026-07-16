---
title: Ship pipeline — PR #776
date: 2026-07-16
pr: 776
issue: 770
outcome: context-assembly
---

# Ship Pipeline — PR #776

**Started:** 2026-07-16

## Pre-flight

- PR #776 on `issue/770`, closing #770.
- Draft PR existed before implementation.
- Canonical computed-but-not-delivered guardrail reviewed; the ship procedure's
  retired metis path is recorded as friction.
- RNA extracted graph is live with 40,554 symbols. LSP caller/reference
  coverage is explicitly degraded by TypeScript initialization.
- Initial CodeRabbit state: skipped while draft.
- Existing static-analysis comments: unused `shlex` import and ambiguous
  adjacent string literals in `make_task_prompt`.

## Step 1: RNA-Grounded Review

**Verdict:** ADJUST

- Metis/guardrails: computed-but-not-delivered, real MCP client, CI artifact
  delivery, CodeRabbit final approval, benchmark isolation.
- Graph impact: Python harness functions are script-local; Rust diagnostic
  construction feeds extracted diagnostic nodes. The UTF-8-safe truncation
  preserves the full metadata message.
- Acceptance criteria are implemented and have an honest resolved manual run,
  with degraded embeddings recorded separately from completed call/reference
  enrichment.
- Findings: remove unused import, make prompt concatenation explicit, and make
  the full-condition capability caveat prominent because the CI artifact lacks
  embeddings.

## Step 2: Independent Code Review

**Initial verdict:** REQUEST CHANGES

The independent reviewer found five acceptance-blocking gaps:

1. duplicate Claude message events inflated stage usage;
2. transcript inference overwrote exact executor stage reports;
3. first-edit-through-exit usage was mislabeled as a handoff interval;
4. handshake-only MCP traffic passed the proof gate;
5. MCP traces omitted request arguments and response identity.

**Final re-review verdict:** APPROVE. All five findings, including the
MCP-native `result.isError: true` edge case found during re-review, are resolved.

## Step 3: Fix

Implemented:

- deduplicate provider usage by stable message ID while retaining the final
  usage record;
- preserve exact executor stage reports over transcript-derived values;
- keep unobserved handoff stages unknown and record the actual post-edit to exit
  interval separately;
- require a successful correlated pre-first-edit RNA tool response;
- retain tool-call params and SHA-256 wire-message hashes in the proxy trace;
- reject both JSON-RPC errors and MCP-native tool results with `isError: true`;
- remove the unused import and implicit string concatenation warnings.
- remove the redundant pre-loop bucket assignment reported after ready review.

Ten harness regression tests pass, including repeated-message, exact-stage
precedence, handshake/error-only rejection, request/response correlation, and
capability-degradation evidence. Replaying the retained successful transcript
now produces deduplicated provider-stage buckets and preserves authoritative
aggregate totals.

## Step 3b: Mark Ready

PR marked ready; CodeRabbit explicitly triggered.

## Step 4: Regression Oracle

Ten Python harness tests and the Rust multibyte regression cover the acceptance
criteria and review findings. Clean GitHub Actions full test job passed.

## Step 5: Merit Assessment

**Verdict:** MERGE. The harness provides reproducible causal evidence rather
than a one-off patch or leaderboard claim.

## Step 6: Resolve TODOs

All findings and static-analysis comments fixed and replied to. No product TODO
remains. The ready-review CodeRabbit pass found five additional issues, all
fixed with regressions: stale MCP docs, non-terminal response correlation,
rename/copy path parsing, descendant process cleanup on timeout, and untracked
file omission from prediction patches.

## Step 7a: Manual Verification

Official evaluator resolved `django__django-13279`: one submitted, one
completed, one resolved, zero errors. Exact isolation, telemetry, MCP, timing,
cost, and degraded-capability evidence are posted on the PR.

## Step 7b: Delivery Verification

Real Claude MCP use and a final proxy protocol smoke both delivered successful
correlated RNA search results through stdio. Node persistence checklist is N/A.

## Step 8: README

README links the new one-instance harness documentation.

## Step 9: Smoke Test

Thirteen Python tests pass. The ready-PR clean CI full test job passes. Local
`cargo test --no-fail-fast` passed every test binary except the known
checkout-history-sensitive roots assertion that reads persisted degraded scan
text from this stateful checkout; #775 independently documented the same local
condition and clean-CI pass.

## Step 10: CI Green

Pending.

## Step 10b: Final Comment Sweep

Pending.

## Step 11: Merge

Pending.
