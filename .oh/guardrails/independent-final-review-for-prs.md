---
id: independent-final-review-for-prs
outcome: agent-alignment
severity: hard
statement: Every PR must receive an explicit APPROVE from a fresh, separate repo-local /review sub-agent on the exact final diff before merge. Any later diff change invalidates approval and requires another fresh reviewer.
---

## Rationale

The quality property is independent adversarial review of the code that will actually merge. Making a third-party reviewer service mandatory couples delivery to external availability and quota without guaranteeing that the reviewed diff is current. A repository-controlled, fresh-context sub-agent review makes the evidence reproducible while preserving reviewer independence.

## Enforcement

After all fixes, verification, CI, and the final comment sweep:

1. Spawn a new reviewer sub-agent that is distinct from the implementer and every earlier reviewer.
2. Give it only the exact final diff and commit SHA, issue acceptance criteria, relevant guardrails/metis, and RNA impact context.
3. Do not give it implementation reasoning, session files, prior conversation, or a defense of the chosen solution.
4. Require a PR comment identifying the reviewer task, reviewed commit, findings, and an explicit `APPROVE` or `REQUEST CHANGES`.
5. Fix every `REQUEST CHANGES` finding and repeat with another fresh reviewer.
6. If the diff changes after approval for any reason, invalidate the approval and repeat the review on the new final commit.

CodeRabbit is optional supplemental feedback. Never trigger or wait for it. If CodeRabbit comments already exist, audit and address actionable findings like any other external review comment.

## Override Protocol

None. The gate applies to code, tests, documentation, configuration, and repository-instruction changes because any merged diff can alter project behavior or governance.

## Evidence

Established by explicit project policy in issue #777 and PR #778 after external reviewer availability blocked otherwise complete delivery.
