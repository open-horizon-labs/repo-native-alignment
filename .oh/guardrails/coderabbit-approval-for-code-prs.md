---
id: coderabbit-approval-for-code-prs
outcome: agent-alignment
severity: hard
statement: Every code-changing PR must receive an explicit clean or approved CodeRabbit review of the final diff before merge. A skipped draft review or green status without review content is insufficient.
---

## Rationale

CodeRabbit only reviews non-draft PRs and can report a successful or skipped status without having reviewed the final code. Treating that status as approval creates a gap in the quality gate precisely when the PR transitions from implementation to merge readiness.

## Enforcement

After marking a code-changing PR ready, explicitly trigger CodeRabbit if it does not start automatically. Resolve every actionable finding, then require a clean or approved review on the final diff before merge.

## Override Protocol

None for code-changing PRs. Documentation-only or repository-instruction-only PRs may mark this gate N/A, with the classification stated in the ship comments.

## Evidence

Established by explicit project policy during PR #749.
