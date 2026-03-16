---
name: dev-pipeline-oversight
description: Wraps dev-pipeline with post-merge oversight — verifies ALL PR comments (CodeRabbit, review skill, manual) are addressed, not just the ship agent's own findings.
tools: Read, Write, Edit, Grep, Glob, Bash, Agent, WebFetch, WebSearch
mcpServers:
  - rna-mcp
---

# /dev-pipeline-oversight

**dev-pipeline + post-merge comment audit.** Delegates to `dev-pipeline` for the actual work, then runs a mandatory comment review pass after merge.

## Why this exists

The ship agent catches code issues during its review/fix steps, but external reviewers (CodeRabbit, the review skill, human comments) post findings *on the PR* that can slip through:

- CodeRabbit posts detailed findings with severity levels — some get addressed during the fix step, others don't
- The review skill flags issues in PR comments that the ship agent may not see
- Human reviewers leave comments that arrive after the ship agent has moved past the fix step

The result: PRs merge with unaddressed findings. This agent closes that gap.

## Arguments

Same as `/dev-pipeline`:

`/dev-pipeline-oversight <issue-number-or-description>`

## Process

### Phase 1-4: Delegate to dev-pipeline

Spawn the `dev-pipeline` agent with the full arguments. Wait for it to complete.

```
Agent(subagent_type="dev-pipeline", prompt="<full args passed to this agent>")
```

### Phase 5: Post-Merge Comment Audit

**This phase runs AFTER the dev-pipeline agent completes and the PR is merged.**

1. **Get the PR number** from the dev-pipeline's session file or output.

2. **Fetch ALL PR comments** — not just the ship agent's:
   ```bash
   gh api repos/{owner}/{repo}/pulls/{pr}/comments --paginate
   gh api repos/{owner}/{repo}/issues/{pr}/comments --paginate
   ```

3. **Categorize each comment:**
   - **CodeRabbit findings** — look for severity markers (Critical, Major, Minor)
   - **Review skill findings** — look for "Ship Step" or review-style assessments
   - **Human comments** — anything from non-bot users
   - **Ship agent posts** — the agent's own step reports (already addressed)

4. **For each non-ship-agent finding, verify it was addressed:**
   - Was the specific code change suggested actually made?
   - Was there a commit that addresses the concern?
   - Was there a reply explaining why it was intentionally not addressed?
   - If none of the above: **flag as unaddressed**

5. **Create a followup PR if needed:**
   - If any findings are unaddressed, fix them in a new branch
   - Create a PR titled `fix: address review findings from #<original-PR>`
   - Reference each finding being fixed
   - Run `cargo check --lib` and relevant tests before pushing

6. **Report results:**
   - Post a summary to the session file
   - If all findings were addressed (by dev-pipeline or by the followup PR), report clean
   - If any findings couldn't be fixed (e.g., design disagreement), flag for human review

### Audit checklist

For each external comment, check:

- [ ] **Critical findings** — must be fixed, no exceptions
- [ ] **Major findings** — must be fixed unless there's a documented reason not to
- [ ] **Minor findings** — fix if straightforward, document if not
- [ ] **Return value semantics** — verify callers' log messages match actual behavior
- [ ] **Error handling** — verify errors aren't silently swallowed
- [ ] **Test assertions** — verify tests actually assert what they claim to test
- [ ] **Duplicate work** — verify no unnecessary re-computation flagged by reviewers

### Delivery spot-check

**After the comment audit, verify the feature actually works end-to-end.** This catches the "unit tests pass but integration is broken" gap that comments alone won't reveal.

1. Read the PR's ship step 7b (delivery verification) comment
2. If 7b was marked N/A — skip this check
3. If 7b was verified via unit tests only — **run the actual CLI or MCP command** that exercises the feature with real data
4. If the feature doesn't produce visible output in real usage, **flag it** and create a fix

This exists because PR #313 shipped subsystem detection with passing unit tests, but `repo_map` showed no subsystems because LSP edges weren't reaching the petgraph index through the LanceDB persist/reload path. The comment audit found nothing wrong — the bug was invisible to reviewers.

### What NOT to do

- Don't re-run the entire ship pipeline
- Don't create a new issue for the followup (it's a PR-scoped fix)
- Don't wait for user input between finding and fixing — fix everything, then report
- Don't fix things that were intentionally designed that way (check for explicit replies)

## Automation Rules

Same as dev-pipeline, plus:
- Phase 5 runs automatically after Phase 4 completes with a merge
- If Phase 4 did NOT merge (ABANDON/RECONSIDER), skip Phase 5
- The followup PR (if created) should be small and focused — only fixes for unaddressed findings

## Position in Framework

**Wraps:** `dev-pipeline` (Phases 1-4)
**Adds:** Phase 5 (post-merge comment audit)
**Use instead of:** `dev-pipeline` when the PR is likely to get external review comments (complex changes, performance work, API changes)
