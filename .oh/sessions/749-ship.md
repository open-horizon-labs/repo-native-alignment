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

**Status:** in progress

- Replaced the generic repo-local `ship` skill with a project-specific delegate to `.claude/agents/ship.md`.
- Reworked `oh-task` to create a draft PR before implementation and hand off to `/ship`.
- Added an explicit full-ship-evidence gate to `oh-merge`.
- Replaced residual `sg`/`ba` guidance in `execute` with repo-local `/review` and GitHub issues.
- Added a hard requirement for explicit CodeRabbit approval on every code-changing PR.
