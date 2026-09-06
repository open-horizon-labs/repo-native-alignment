---
id: reverify-plan-semantics-against-head
title: "Re-verify Plan-Stage Behavioural Notes Against HEAD Before Encoding Them"
outcome: context-assembly
source_issue: 873
---
An issue's root-cause analysis and a solution-space plan are snapshots of the
code at the moment they were written. By the time `/execute` starts, sibling
PRs may have landed and silently changed the semantics the plan promises to
preserve. "Edge-for-edge identical" is then a claim about the wrong baseline.

Issue #873 was written against Pass 1 code that PR #800 had already half-replaced:
`EndpointLookupIndex` removed two of the three quadratic branches before the
fix branch existed. The dev session caught that. It did not catch that the
plan's subtlety "duplicate NodeIds -> two entries -> ambiguous" was also
pre-#800: at HEAD, `EndpointLookupIndex::build` sorts and dedups by stable id,
so same-named functions in one file resolve to ONE target. The ship pipeline
found it only because a regression test written from the plan text failed.

Before executing against a plan:
1. Re-read every "behaviour that must be preserved" note against HEAD, not
   against the issue text. Name the function that implements it today.
2. Write the regression test for each note *first*. A failing test on
   unchanged code is a signal to investigate, not a verdict: the note may be
   stale, the test may be wrong, or HEAD may have a real defect. Decide which
   with evidence (the implementing function, the PR that changed it) before
   touching the plan.
3. When a note is stale, update the session file and the PR description so
   reviewers evaluate the equivalence claim against the real baseline.
4. Treat merged PRs that touch the same files since the issue was filed as
   required reading (`git log --since=<issue-date> -- <files>`).
