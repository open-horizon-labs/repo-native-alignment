---
id: review-readiness-skill-rationale
outcome: context-assembly
title: 'Review Readiness Starts With Cheap Diff Graph Context, Not Full Enrichment'
---

## Pattern

PR review is a workflow-specific context job. It does not need full-repo LSP or embeddings before it can start, but it also does not need a rigid generated report for every diff. A useful skill starts from the review decision, then pulls cheap context only where it changes what the reviewer should inspect.

The `/rna-mcp:review-readiness` skill is a walking skeleton for capability-scoped readiness as an agent process. It helps an agent decide:

- whether raw `git diff` is already sufficient
- which changed symbols/files/outcomes/guardrails deserve review attention
- which existing RNA graph or business context changes the review decision
- which specific metadata is missing, stale, or intentionally not needed

## Why this matters

This avoids treating LSP enrichment as the product, and avoids treating a helper script as the skill. LSP is a possible follow-up provider for exact callers/references, not a prerequisite for all review context. The skill should say when RNA adds no material value for a diff rather than forcing graph-shaped output.

This directly supports #659: enrichment readiness should be workflow-specific. For PR review, the first useful answer is often a human-readable review map: what changed, what context matters, what is missing, and whether to continue reviewing.

## When to use

- Before reviewing a PR or dirty working tree.
- Before deciding whether RNA context, targeted LSP impact lookup, or plain diff review is the right next step.
- When an agent needs to explain what context would improve review confidence.

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
