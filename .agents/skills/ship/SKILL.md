---
name: ship
description: Run RNA's mandatory project-specific quality gate from completed implementation through review, verification, CI, and merge. Use when a draft PR is ready to be delivered.
---

# /ship

Run the repository's complete RNA shipping pipeline. This repo-local skill intentionally delegates to the detailed project procedure instead of using the generic Open Horizons delivery workflow.

## Required procedure

1. Read `.claude/agents/ship.md` completely before taking shipping actions.
2. Execute every step in that file sequentially against the target PR.
3. Post every required finding and verification result to the PR.
4. Do not collapse, skip, or reorder gates.
5. Do not merge unless the final acceptance-criteria and comment-sweep gates pass.

The authoritative definition is also summarized in `AGENTS.md`. If generic Open Horizons guidance conflicts with the RNA pipeline, the RNA pipeline wins.

## Invocation

`/ship <PR-number>`

If no PR number is supplied, resolve the PR from the current branch.

## Completion

Report the merged PR URL and post-merge verification result. If any mandatory gate blocks, leave the PR open and report the exact blocking step.
