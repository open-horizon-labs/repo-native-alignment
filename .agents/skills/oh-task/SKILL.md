---
name: oh-task
description: Claim a GitHub issue, create its branch and draft PR before implementation, execute the work in isolation, and hand the completed draft to RNA's full ship gate.
---

# oh-task

Work a GitHub issue through the repository's mandatory draft-PR-first workflow.

## Invocation

`/oh-task <issue-number> [base-branch]`

The base defaults to `main`. Use another branch only for an intentional stacked PR.

## Flow

1. Load `AGENTS.md`, relevant `.oh/` artifacts, and RNA context.
2. Fetch and validate the open issue. Stop if it is closed, owned by someone else, or lacks enough information to proceed.
3. Claim the issue with `gh issue edit <issue> --add-assignee @me`.
4. Run the repo-local `/aim`, `/problem-space`, `/problem-statement`, and `/solution-space` phases at the altitude required by the issue. Record the chosen solution.
5. Create an isolated branch/worktree from the selected base.
6. **Before changing implementation files**, push the branch and open a draft PR:

   ```bash
   git push -u origin issue/<number>
   gh pr create --draft --base <base-branch> --title "<issue-title>" --body "Closes #<number>

   ## Problem
   <problem statement>

   ## Chosen solution
   <solution-space recommendation>"
   ```

7. Execute the chosen solution. Keep all work within the declared issue scope.
8. Run project checks and the repo-local `/review` skill. Fix every actionable finding.
9. Commit focused changes tagged with the relevant `[outcome:X]` and push them to the draft PR.
10. Complete any descendant issues created during execution before declaring the implementation complete.
11. Invoke repo-local `/ship <PR-number>`. That pipeline alone marks the PR ready, handles CI, verifies delivery, obtains explicit approval of the final diff from a fresh, separate repo-local `/review` sub-agent, and merges.
12. Clean up the worktree only after `/ship` completes or reports a blocker.

## Guardrails

- Never implement before the draft PR exists.
- Never mark the PR ready or merge directly from this skill.
- Never substitute a lightweight review for the full RNA `/ship` pipeline.
- Never merge without an explicit `APPROVE` from a fresh, separate repo-local `/review` sub-agent on the current final diff. Any later diff change invalidates approval and requires a new fresh reviewer.
- Never trigger or wait for CodeRabbit. If actionable CodeRabbit comments already exist, address them like any other external review feedback.
- Use `AGENTS.md`, `.oh/`, and RNA for context; do not depend on external task-memory tools.
- If completion signaling is unavailable, report `COMPLETION: status=<status> pr=<url>`.

## Exit conditions

- **Success:** `/ship` merged the PR and verified delivery.
- **Blocked:** a human decision or mandatory ship gate prevents progress.
- **Error:** the issue, branch, PR, or verification workflow cannot be completed safely.
