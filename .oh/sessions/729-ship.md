---
session: 729-ship
issues: [728]
pr: 729
branch: issue/728-release-blockers
date: 2026-07-14
tags: [context-assembly, local-knowledge, persistence, ship]
---

# Ship Pipeline — PR #729

## Pre-flight

- PR: #729 `Fix local-knowledge release blockers`
- Branch: `issue/728-release-blockers`
- Issue: #728
- Delivery path: clean worktree -> draft PR -> ship quality gate -> ready PR/CI/CodeRabbit -> squash merge to main -> exact successful Actions artifact -> installed binary/MCP and release-suite verification.
- Required project context: injected `AGENTS.md`, `/execute`, `/ship`, `.claude/agents/ship.md`, and the promoted `computed-but-not-delivered` guardrail.
- RNA scan gate: live worktree index refreshed by the candidate-binary release suite.
- Initial CodeRabbit state: skipped while draft; no inline review comments before ready.
- Candidate verification before ship: 22 targeted tests and release suite 141/0/0.

## Delivery-Path Tax

- Cold Rust test linking took 13 minutes despite a warm check build.
- The ship runbook points to a retired metis path; the current rule lives as a guardrail.
- Exact-artifact verification must wait for ready-state CI and download the artifact for the final commit.

## Step 1: RNA-Grounded Review

**Verdict:** ADJUST

- The base symbols schema persists `extraction_source`, but the duplicated vector schema omits it.
- The schema regression test covers only the base schema, allowing the vector schema to drift.
- The PR description still says the branch is a scaffold even though implementation and candidate evidence are pushed.

## Step 2: Independent Code Review

**Verdict:** REQUEST CHANGES

- Confirmed the vector symbols schema omission.
- Found that MCP search and traversal did not render `Node.source`, so the release fixture could not distinguish persisted Markdown provenance from a `TreeSitter` fallback.

## Step 3: Fix

- Added `extraction_source` to `symbols_schema_with_vector` and a parity regression assertion.
- Added extraction-source rendering to compact/full unified-search output and the legacy graph service formatter.
- Extended the artifact suite with explicit search and traversal provenance checks.

**Fix verification:** schema parity test passed; compact/full formatter test passed; candidate-binary release suite passed 143/0/0 after an intentional red run exposed the separate legacy graph formatter.
