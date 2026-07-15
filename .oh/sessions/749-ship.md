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

**Status:** complete; awaiting final-diff re-review

- Replaced the generic repo-local `ship` skill with a project-specific delegate to `.claude/agents/ship.md`.
- Reworked `oh-task` to create a draft PR before implementation and hand off to `/ship`.
- Added an explicit full-ship-evidence gate to `oh-merge`.
- Replaced residual `sg`/`ba` guidance in `execute` with repo-local `/review` and GitHub issues.
- Added a hard requirement for explicit CodeRabbit approval on every code-changing PR.
- Addressed all 17 CodeRabbit findings, including final-SHA validation after rebase, `/ship` and CodeRabbit gates in `oh-join`, RNA-first exploration, current MCP tool names, verification failure propagation, and human approval before persisting metis.
- Applied the requested Markdown fence/table cleanup and revalidated all repo-local skills.

## Steps 4–8: Quality and Delivery

- Regression oracle: 8 workflow assertions passed.
- Merit verdict: MERGE.
- All review TODOs are addressed without silent deferral.
- Fresh-checkout manual verification passed.
- Real MCP protocol smoke passed with all 4 required tools visible.
- README update is not applicable because no product capability or CLI behavior changed.

## Step 9: Tests and CI

- GitHub Rust CI, lint, audit, test, CodeQL, and checklist checks passed for commit `159781d8`.
- Local library suite: 1,952 passed, 2 ignored, 0 failed.
- One environment-sensitive integration assertion failed locally because the installed real `rust-analyzer` bypassed the test's simulated failure; the same integration suite passed in clean GitHub CI. No Rust source changed in this PR.
- A final CI and CodeRabbit review are required after the review-fix commit is pushed.
