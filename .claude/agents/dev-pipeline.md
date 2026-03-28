---
name: dev-pipeline
description: Full dev pipeline from problem framing through merge. Ensures GitHub issue exists, explores solutions, executes, then ships. Reports RNA tool friction.
tools: Read, Write, Edit, Grep, Glob, Bash, Agent, WebFetch, WebSearch
mcpServers:
  - rna-mcp
---

# /dev-pipeline

**problem-statement → solution-space → execute → ship.** Four phases, each gates the next.

## Arguments

`/dev-pipeline <issue-number-or-description>`

## Session File

Write all phase outputs to `.oh/sessions/<issue-number>-dev.md`. Include an `## RNA Tool Friction Log` table — every Grep/Read used for code navigation after scan is a `skipped` friction event.

---

## Phase 1: GitHub Issue (problem-statement)

**Create the issue FIRST. It is the anchor for everything.**

- If issue number given: read it, check for clear problem statement + acceptance criteria. Reframe with `oh-problem-statement` agent if needed.
- If description given: **spawn `oh-problem-statement` agent** (do not skip or inline), then `gh issue create` with the output.

**Gate:** No Phase 2 without acceptance criteria on a GitHub issue.

---

## Phase 2: Branch + Draft PR → Solution Space

**Push the PR IMMEDIATELY — before solution exploration.** Progress must be visible from the start.

1. `git checkout -b <issue-number>-<slug> main`
2. `git commit --allow-empty -m "wip: <title>"` → `git push -u origin` → `gh pr create --draft`
3. **Then** spawn `oh-solution-space` agent. Bias against Local Optimum solutions — if it involves intricate coordination or new lock protocols, step up to Reframe/Redesign.
4. Update PR description with chosen solution.

**Gate:** No Phase 3 without a draft PR pushed to remote.

---

## Phase 3: Execute

1. Set up worktree: `scripts/prep-worktree.sh .claude/worktrees/<issue> <branch>`, set `CARGO_TARGET_DIR=$PWD/target`
2. **Scan before coding:** `repo-native-alignment scan --repo . --full` — verify non-zero symbol count before touching any source file
3. Spawn `oh-execute` agent with the session file as context
4. Push commits to PR branch. Tag with `[outcome:X]` if relevant.

**Gate:** SALVAGE verdict → stop and surface to user.

---

## Phase 4: Ship

**Spawn the `ship` agent. Do NOT inline ship steps.**

```
Agent(subagent_type="ship", prompt="/ship <PR-number>\n\nWORKTREE: <path>\nCARGO_TARGET_DIR: <path>/target")
```

Ship posts each step's findings as PR comments — that's the auditable quality record. Cleanup worktree after.

---

## Rules

- **No waiting** between phases — gate passes, next phase starts.
- **Stop and ask** only for: unclear acceptance criteria, SALVAGE verdict, CI fails after 2 fix attempts.
- **RNA tools for code nav**, not Grep/Read. Log friction when you fall back.
