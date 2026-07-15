---
type: session
status: active
outcome: agent-alignment
pr: 749
---

# Ship Pipeline — PR #749

**Started:** 2026-07-15

## Step 1: RNA-Grounded Review

**Verdict:** ADJUST

RNA index: 18,236 symbols, schema v23. No product symbols changed; impact is procedural because repo-local skills become authoritative for future agents.

Metis and guardrails checked: computed-but-not-delivered, repo-native, dogfood-rna-tools, draft-PR-before-execute, and the project-specific full ship gate.

Findings:

1. `oh-task` created PRs after implementation instead of opening a draft PR before execution.
2. `oh-merge` could merge without evidence that the full RNA `/ship` gate completed.
3. The vendored generic `ship` skill took repo-local precedence without implementing RNA's mandatory project pipeline.

All three findings are being fixed before the PR is marked ready.

## Step 2: Independent Code Review

**Verdict:** REQUEST CHANGES

The independent reviewer confirmed the generic `ship` conflict and found residual `sg`/`ba` references in the core `execute` skill.

## Step 3: Fix

**Status:** pending final-diff re-review

- Replaced the generic repo-local `ship` skill with a project-specific delegate to `.claude/agents/ship.md`.
- Reworked `oh-task` to create a draft PR before implementation and hand off to `/ship`.
- Added an explicit full-ship-evidence gate to `oh-merge`.
- Replaced residual `sg`/`ba` guidance in `execute` with repo-local `/review` and GitHub issues.
- Added a hard requirement for explicit CodeRabbit approval on every code-changing PR.
- Addressed all 17 CodeRabbit findings, including final-SHA validation after rebase, `/ship` and CodeRabbit gates in `oh-join`, RNA-first exploration, current MCP tool names, verification failure propagation, and human approval before persisting metis.
- Applied the requested Markdown fence/table cleanup and revalidated all repo-local skills.

## Step 3b: Mark Ready

The draft PR was marked ready, which triggered Rust CI and explicit CodeRabbit
review. Every CodeRabbit change request is handled as a blocking Step 3 finding.

## Step 4: Regression Oracle

Eight executable workflow assertions passed, covering skill validation,
forbidden dependencies, draft-PR ordering, full-ship delegation, merge gates,
CodeRabbit policy, portable MCP configuration, and RNA startup.

## Step 5: Merit Assessment

Verdict: MERGE. The repo now carries its own agent workflow, RNA configuration,
and enforceable review gates without changing product runtime code.

## Step 6: Resolve TODOs

All Step 1, Step 2, and CodeRabbit findings were fixed. Nothing was silently
deferred; follow-up reviews were explicitly requested after every fix commit.

## Step 7a: Manual Verification

A fresh clone contained the repo-local skills and portable MCP configuration;
the configured RNA command resolved and started successfully.

## Step 7b: Delivery Verification

The real MCP protocol smoke passed and exposed all four required tools. Product
metadata persistence checks are N/A because no graph metadata changed.

## Step 8: README

N/A with rationale: no user-facing RNA product capability, CLI behavior, or flag
changed. Repository workflow documentation lives in `AGENTS.md` and the skills.

## Step 9: Smoke Test

The local library suite passed 1,952 tests with 2 ignored and 0 failures. One
environment-sensitive integration assertion failed because the installed real
`rust-analyzer` bypassed its simulated failure; the same integration test passed
in clean GitHub CI. No Rust source changed in this PR.

## Step 10: CI Green

Rust CI, lint, audit, test, CodeQL, and checklist checks passed on each pushed
review-fix commit. Final CI must also pass on the exact merge SHA.

## Step 10b: Final Comment Sweep

Every external comment has been fetched with pagination and addressed. The final
exact-SHA CodeRabbit review remains the last pre-merge gate; a status check alone
does not qualify.

## Step 11: Merge

Acceptance criteria are satisfied and the PR is mergeable. Squash merge is
authorized only after Step 10 and Step 10b pass on the final SHA. The post-merge
PR comment is the truthful evidence for this terminal step; it cannot be recorded
as completed in a commit before the merge occurs.
