---
name: dev-pipeline-oversight
description: Wraps dev-pipeline with post-merge oversight — verifies ALL PR comments (CodeRabbit, review skill, manual) are addressed, not just the ship agent's own findings.
tools: Read, Write, Edit, Grep, Glob, Bash, Agent, WebFetch, WebSearch
mcpServers:
  - rna-mcp
---

# /dev-pipeline-oversight

**dev-pipeline + post-merge comment audit.** Catches findings that slip through the ship agent.

## Arguments

Same as dev-pipeline: `/dev-pipeline-oversight <issue-number-or-description>`

## Process

### Phase 1-4: Delegate to dev-pipeline

```
Agent(subagent_type="dev-pipeline", prompt="<full args>")
```

**Early verification:** Confirm within the first few minutes that a GitHub issue AND a draft PR were pushed. If the agent starts coding without either, stop it — that's a process failure.

### Phase 5: Post-Merge Comment Audit

Runs AFTER Phase 4 merges. Skip if Phase 4 did not merge.

1. Fetch ALL PR comments:
   ```bash
   gh api repos/{owner}/{repo}/pulls/{pr}/comments --paginate
   gh api repos/{owner}/{repo}/issues/{pr}/comments --paginate
   ```

2. For each non-ship-agent comment, verify it was addressed:
   - Code change made? Commit that fixes it? Reply explaining why not?
   - If none: **unaddressed**.

3. Priority: Critical=must fix. Major=must fix or document why not. Minor=fix if easy.

4. If anything unaddressed: fix in a new branch `fix: address review findings from #<PR>`, push PR.

### Delivery spot-check

After comment audit, run the actual CLI/MCP command that exercises the feature with real data. Unit tests passing ≠ integration working.

### What NOT to do

- Don't re-run the ship pipeline
- Don't create new issues for the followup (it's PR-scoped)
- Don't wait for user input between finding and fixing
