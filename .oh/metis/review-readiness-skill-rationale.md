---
id: review-readiness-skill-rationale
outcome: context-assembly
title: 'Review Readiness Starts With Cheap Diff Graph Context, Not Full Enrichment'
---

## Pattern

PR review is a workflow-specific context job. It does not need full-repo LSP or embeddings before it can start. A useful first pass can come from cheap sources: git diff hunks, current extracted symbol ranges, and existing RNA graph metadata.

The `/rna-mcp:review-readiness` skill is a walking skeleton for capability-scoped readiness. It reports what is ready for the review job and what is explicitly missing:

- changed files/hunks from git diff
- changed-symbol overlap from extracted graph ranges
- existing graph context such as stable node IDs and edge counts
- file/hunk-level representation for unmapped or deleted changes
- readiness gaps for exact semantic refs, deleted symbol identity, and embeddings

## Why this matters

This avoids treating LSP enrichment as the product. LSP is a possible follow-up provider for exact callers/references, not a prerequisite for all review context.

This directly supports #659: enrichment readiness should be workflow-specific. For PR review, the first useful answer is often cheap and available now; for global dead-code, the required coverage is much stricter.

## When to use

- Before reviewing a PR or dirty working tree.
- Before deciding whether targeted LSP impact lookup is worth running.
- When an agent needs to know whether existing graph context beats raw `git diff` for a change.

## When not to use

- To claim global dead-code safety.
- To prove exact incoming references/callers.
- To identify deleted symbols without base-side symbol data.
- To replace compiler/test verification.

## References

- Skill: `plugin/skills/review-readiness/SKILL.md`
- Parent issue: #659
- Child issue: #660
- Session: `.oh/sessions/659-capability-scoped-enrichment-readiness.md`
