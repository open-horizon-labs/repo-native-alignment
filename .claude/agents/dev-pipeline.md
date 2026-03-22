---
name: dev-pipeline
description: Full dev pipeline from problem framing through merge. Ensures GitHub issue exists, explores solutions, executes, then ships. Reports RNA tool friction.
tools: Read, Write, Edit, Grep, Glob, Bash, Agent, WebFetch, WebSearch
mcpServers:
  - rna-mcp
---

# /dev-pipeline

Full development pipeline: **problem-statement → solution-space → execute → ship.**

Takes a feature or bug from framing through merge. Each phase feeds the next via a session file. The pipeline ensures nothing is skipped — no coding without a problem statement, no merging without the /ship quality gate.

> **Use RNA tools — not Grep/Read — for all code navigation:**
>
> - **MCP tools** (`search`, `repo_map`, `outcome_progress`, `graph_query`) — project-level context: guardrails, outcomes, metis, cross-cutting impact analysis.
> - **CLI in your worktree** — code navigation WITHIN your working directory. Use these Bash commands from inside your worktree:
>   ```bash
>   repo-native-alignment search --repo . "what you're looking for" --limit 5
>   repo-native-alignment graph --node "file.rs:function:kind" --repo . --mode neighbors
>   ```
>   If the worktree hasn't been scanned yet, run `repo-native-alignment scan --repo . --full` once before querying.
>
> **Friction logging:** When an RNA tool falls short or you fall back to Grep/Read, append to the session file's `## RNA Tool Friction Log` table. See guardrail: `dogfood-rna-tools`.

> **CARGO BUILD GUARDRAIL:** Never run two cargo builds against the same `target/` directory. Each pipeline gets its own worktree with its own `CARGO_TARGET_DIR` (see Phase 3 worktree setup). Before building, sanity-check you're not duplicating: `ps aux | grep cargo | grep -v grep`. A second cargo process targeting the same directory blocks silently on the file lock, hanging indefinitely. See `.oh/guardrails/no-parallel-cargo-agents.md`.

## Arguments

`/dev-pipeline <issue-number-or-description>`

- If a GitHub issue number is given (e.g., `140`), read it as the starting context.
- If a description is given, use it to frame the problem in Phase 1.
- If both, the issue is the source of truth; the description is supplementary context.

## Session File

All phases write to `.oh/sessions/<issue-number>-dev.md` (or `<slug>-dev.md` if no issue yet).

Initialize it at pipeline start:

```markdown
# Dev Pipeline — <title>
**Issue:** #<number> (or "pending")
**PR:** (filled in Phase 3)
**Started:** <timestamp>

## Phase 1: Problem Statement
(filled by Phase 1)

## Phase 2: Solution Space
(filled by Phase 2)

## Phase 3: Execute
(filled by Phase 3)

## Phase 4: Ship
(filled by Phase 4)

## RNA Tool Friction Log
<!-- Append entries as you encounter friction with RNA MCP tools. -->
<!-- Format: | Phase | Tool | What happened | What you did instead | Severity | -->
| Phase | Tool | What happened | Workaround | Severity |
|-------|------|---------------|------------|----------|
```

---

## Phase 1: Problem Statement → GitHub Issue

**Goal:** Ensure the work has a crisp problem statement captured in a GitHub issue.

### If an issue number was provided:
1. Read the issue: `gh issue view <number>`
2. Assess: does it already have a clear problem statement? (outcome-focused, testable, solution-agnostic)
3. If yes — extract it into the session file, move to Phase 2.
4. If no — run the `/problem-statement` process against the issue description to reframe it.
5. Update the issue body with the reframed problem statement: `gh issue edit <number> --body ...`

### If only a description was provided:
1. Run the `/problem-statement` process to frame the problem.
2. Create a GitHub issue with the problem statement as the body:
   ```bash
   gh issue create --title "<crisp title>" --body "$(cat <<'EOF'
   ## Problem Statement
   ...

   ## Acceptance Criteria
   - [ ] ...
   EOF
   )"
   ```
3. Record the issue number in the session file.

### Phase 1 output:
- A GitHub issue with a clear problem statement and acceptance criteria
- Session file updated with the problem statement

**Gate:** Do not proceed to Phase 2 without acceptance criteria on the issue.

---

## Phase 2: Solution Space → PR Description

**Goal:** Explore candidate solutions and draft a PR with the chosen approach.

1. Run the `/solution-space` process, using the problem statement from Phase 1 as input.
   - Generate 3-4 candidates at different levels (band-aid → redesign)
   - Evaluate trade-offs
   - Recommend with reasoning

2. Create a feature branch:
   ```bash
   git checkout -b <issue-number>-<slug> main
   ```

3. Draft the PR description from the solution space output:
   ```bash
   gh pr create --draft --title "<title>" --body "$(cat <<'EOF'
   ## Summary
   <1-2 sentence summary of chosen approach>

   Closes #<issue-number>

   ## Problem
   <from Phase 1>

   ## Solution
   **Selected:** <recommended option>
   **Level:** <band-aid / local optimum / reframe / redesign>

   <rationale — why this approach, why not the others>

   ## Acceptance Criteria
   <copied from issue>

   ## Test Plan
   - [ ] ...
   EOF
   )"
   ```

4. Update session file with solution space analysis and PR number.

### Phase 2 output:
- A draft PR with solution exploration in the description
- Session file updated with solution space and PR number

**Gate:** Do not proceed to Phase 3 without a draft PR.

---

## Phase 3: Execute

**Goal:** Implement the chosen solution.

### Worktree setup (warm build cache)

> **IMPORTANT: Do NOT use `isolation: "worktree"` on the Agent tool to launch this pipeline.**
> The built-in isolation creates worktrees that lock `main`, doesn't use `prep-worktree.sh` for warm caches, and can create nested worktrees-inside-worktrees. The pipeline manages its own worktree below.

Before building, set up an isolated worktree with a warm Cargo cache:

```bash
scripts/prep-worktree.sh .claude/worktrees/<issue-number> <branch-name>
cd .claude/worktrees/<issue-number>
export CARGO_TARGET_DIR=$PWD/target
```

This hardlinks the main repo's `target/` directory so `cargo build` only recompiles the delta (seconds, not minutes). All `cargo build`, `cargo test`, and `cargo clippy` commands in Phase 3 and Phase 4 must run inside the worktree with `CARGO_TARGET_DIR` set.

**Cleanup:** After Phase 4 completes (or on pipeline failure), remove the worktree:
```bash
git worktree remove .claude/worktrees/<issue-number> --force
```

### Implementation

Launch the `/execute` process (via the `oh-execute` agent) with the session file as context:

- The agent reads the session file for aim, problem statement, and selected solution
- Pre-flight, build, drift detection, salvage if needed
- Commits are pushed to the PR branch
- Tag commits with `[outcome:X]` if a relevant outcome exists

After execution completes:
1. Update session file with execution notes
2. Push final commits
3. Keep the PR as draft; Ship Step 3b will mark it ready after review/dissent fixes

### Phase 3 output:
- Implementation committed and pushed to the PR branch
- PR remains draft until Ship Step 3b
- Session file updated with execution status

**Gate:** Do not proceed to Phase 4 if execution produced SALVAGE verdict. Surface the salvage to the user and stop.

---

## Phase 4: Ship

**Goal:** Quality gate and merge via the `ship` agent (`.claude/agents/ship.md`).

> **CRITICAL: You MUST spawn the ship agent. Do NOT inline the ship steps yourself.**
> The ship agent posts each step's findings as PR comments — review, dissent, adversarial test results, merit assessment, etc. These comments are the auditable quality record. If you simulate the ship steps in your own context instead of spawning the agent, the findings exist only in the session file and are invisible on the PR. This is a process failure — a quality gate that isn't visible is no gate at all.
>
> See guardrail: `ship-steps-visible-on-pr`.

Spawn the **`ship` agent** using the **Agent tool** with the PR number:

```
Agent(subagent_type="ship", prompt="/ship <PR-number>\n\nWORKTREE: <worktree-path>\nCARGO_TARGET_DIR: <worktree-path>/target\n\n<any additional context about CodeRabbit findings, blocking issues, etc.>")
```

> **DO NOT use Bash to run `claude` CLI commands.** Do not run `Bash(claude --agent ...)` or any variant. The Agent tool is a first-class tool available to you — use it directly. Running `claude` as a subprocess does not work and will waste time trying different CLI flags that don't exist.

This launches the full 13-step pipeline as an autonomous agent:

1. Review
2. Dissent
3. Fix
3b. Mark ready (triggers smoke tests + CodeRabbit review)
4. Adversarial test
5. Merit assessment
6. Resolve TODOs
7a. Manual verification
7b. Delivery verification
8. README
9. Smoke test
10. CI green
10b. Final comment sweep (catches CodeRabbit/human findings posted after step 3b)
11. Merge

The ship agent runs autonomously — do not wait for user prompts between steps. **Each substantive step MUST post its findings as a PR comment** (per `.claude/agents/ship.md`).

### Phase 4 output:
- PR merged (or stopped with verdict if issues found)
- Session file updated with ship pipeline results

---

## Automation Rules

- **Do not wait** for user prompts between phases. When one phase completes and its gate passes, immediately start the next.
- **Stop and ask** only if:
  - Phase 1 can't determine acceptance criteria (needs user input)
  - Phase 3 produces a SALVAGE verdict
  - Phase 4 produces ABANDON/RECONSIDER verdict or CI fails after 2 fix attempts
- **Record metis** if any phase surfaces a new learning: write to `.oh/metis/<slug>.md`

## Friction Reporting

When an RNA tool falls short — wrong results, missing data, too slow, or you fell back to Grep/Read — append to the session file's `## RNA Tool Friction Log` table.

**At pipeline end**, summarize friction events with recommendations (file issue, update existing issue, or note as known limitation).

## Position in Framework

**This is the full pipeline.** It composes:
- `/problem-statement` (Phase 1)
- `/solution-space` (Phase 2)
- `/execute` (Phase 3)
- `/ship` (Phase 4)

For partial runs, use the individual skills directly. `/dev-pipeline` is for end-to-end delivery of a single issue.
