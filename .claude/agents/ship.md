---
name: ship
description: RNA delivery pipeline. 11-step quality gate from implementation to merge, with delivery verification.
tools: Read, Write, Edit, Grep, Glob, Bash, Agent
mcpServers:
  - rna-mcp
---

# RNA /ship Pipeline

The full quality gate for this project. Run sequentially — each step must complete before the next begins. **Do not wait for user prompts between steps.** When one step completes, immediately start the next.

## Arguments

`/ship <PR-number>` — run the pipeline against a specific PR.

If no PR number given, detect from the current branch (`gh pr list --head $(git branch --show-current)`).

## Pre-flight

Before starting:
1. Read AGENTS.md for current project patterns and constraints
2. Read `.oh/metis/computed-but-not-delivered.md` — the metis that created step 7b
3. Identify the PR, branch, and issue being closed
4. Read the PR description and issue acceptance criteria

## The 11 Steps

### 1. /review

Check implementation against acceptance criteria, AGENTS.md patterns, and guardrails.

**How:** Invoke the `/review` skill (or apply its process directly):
- **List every acceptance criterion** from the linked issue. Check each one against the implementation. If any are unmet, they become fix items for step 3.
- Restate the original aim
- Check: still necessary? still aligned? still sufficient? mechanism clear? changes complete?
- Detect drift (scope, solution, goal)
- Verdict: Continue / Adjust / Pause / Salvage

**Use RNA tools:** `oh_search_context(query, artifact_types: ["guardrail"])` to check against relevant guardrails.

**Post findings as PR comment:**
```bash
gh pr comment <PR> --body "$(cat <<'EOF'
## Ship Step 1: Review
**Verdict:** [CONTINUE/ADJUST/PAUSE/SALVAGE]

### Acceptance Criteria
- [x/blank] criterion 1
- [x/blank] criterion 2

### Alignment Check
[findings]
EOF
)"
```

### 2. /dissent

Seek contrary evidence. Devil's advocate pass.

**How:** Invoke the `/dissent` skill (or apply its process directly):
- Steel-man the current approach
- Seek contrary evidence (3+ points)
- Pre-mortem (3 failure scenarios)
- Surface hidden assumptions
- Verdict: PROCEED / ADJUST / RECONSIDER

**Use RNA tools:** `oh_search_context("risks constraints", artifact_types: ["guardrail", "metis"])` to ground the dissent.

**Post findings as PR comment:**
```bash
gh pr comment <PR> --body "$(cat <<'EOF'
## Ship Step 2: Dissent
**Verdict:** [PROCEED/ADJUST/RECONSIDER]

### Contrary Evidence
1. ...

### Pre-Mortem
1. ...

### Hidden Assumptions
| Assumption | Risk if Wrong |
|------------|---------------|
| ... | ... |
EOF
)"
```

### 3. Fix

Address every finding from review + dissent. No deferred items.

If review or dissent found issues, fix them now. Commit with descriptive messages. If nothing to fix, skip.

### 4. Adversarial test

Dissent-seeded tests that try to break the implementation.

**Seed from dissent findings** — the dissent tells you where the implementation was already challenged. Write tests that attack those specific weaknesses. Prioritize: functional > integration > unit.

**Post test results as PR comment:**
```bash
gh pr comment <PR> --body "$(cat <<'EOF'
## Ship Step 4: Adversarial Test
[test results, seeded from dissent finding X]
EOF
)"
```

### 5. Merit assessment

Is this worth merging? Run real queries, compare before/after.

Verdict: MERGE / MERGE WITH CAVEATS / ABANDON / NEEDS MORE WORK.

**Post verdict as PR comment:**
```bash
gh pr comment <PR> --body "$(cat <<'EOF'
## Ship Step 5: Merit Assessment
**Verdict:** [MERGE/MERGE WITH CAVEATS/ABANDON/NEEDS MORE WORK]
[reasoning]
EOF
)"
```

### 6. Resolve TODOs

Every TODO, caveat, and "needs more work" item on the PR must be either:
- **Fixed** — with a commit, or
- **Explicitly marked N/A** with reasoning, or
- **Filed as a follow-up issue** with a link

No silent deferrals.

### 7a. Manual verification (computation)

Run the actual feature with real data. Not unit tests — real queries, real files, real output.

**For RNA:** Use `cargo test` and write integration-style tests that parse real source files and verify the feature produces correct results.

**Post results as PR comment:**
```bash
gh pr comment <PR> --body "$(cat <<'EOF'
## Ship Step 7a: Manual Verification
[real data test results]
EOF
)"
```

### 7b. Delivery verification (NEW — from computed-but-not-delivered metis)

**Verify the feature is visible to agents through MCP tools.**

This step exists because PR #137 taught us: computing a value is not delivering it. New node metadata must survive 3 layers: extraction → LanceDB persistence → MCP tool output rendering.

**Checklist (for any feature that adds/changes data visible to agents):**

- [ ] **Persist:** If new metadata on Node, is there a typed Arrow column in `symbols_schema()` (store.rs)?
- [ ] **Write path:** Is the metadata written to the Arrow batch in BOTH construction sites in server.rs (initial + upsert)?
- [ ] **Read path:** Is the metadata read back from Arrow into Node.metadata during load?
- [ ] **Render:** Does the value appear in ALL relevant MCP tool outputs?
  - `search_symbols` formatting
  - `graph_query` / `format_neighbor_nodes` formatting
  - `oh_search_context` code results formatting
- [ ] **End-to-end:** After `cargo install --path .` + restart + rescan, does the value appear in actual tool output?

**If the PR doesn't add agent-visible data, mark this step N/A.**

**Post checklist results as PR comment:**
```bash
gh pr comment <PR> --body "$(cat <<'EOF'
## Ship Step 7b: Delivery Verification
- [x/blank] Persist: Arrow column in symbols_schema
- [x/blank] Write path: both batch sites
- [x/blank] Read path: Arrow → Node.metadata
- [x/blank] Render: search_symbols, graph_query, oh_search_context
- [x/blank] End-to-end: value visible in tool output after install+restart
EOF
)"
```

### 8. README

Update README.md for any new capability, changed behavior, or new flags.

If no user-facing changes, skip.

### 9. Smoke test

`cargo test` must pass. All tests, not just new ones.

If there's a `src/smoke.rs`, update it to exercise the new code path.

### 10. CI green

Verify CI passes on the final commit: `gh pr checks <PR>`.

If CI is pending, wait. If CI fails, fix and re-run from step 9.

### 11. Merge

**Pre-merge gate: acceptance criteria.**

Before merging, re-read the linked issue's acceptance criteria. Every checkbox must be checked off — either verified done or explicitly filed as a follow-up issue with a link. If any criterion is unmet and not deferred, **do not merge**. Go back to step 3 (fix).

```bash
# Check off criteria on the issue
gh issue edit <ISSUE> --body "$(updated body with [x] marks)"
# Then merge
gh pr merge <PR> --squash --delete-branch
```

## Step Questions (don't collapse steps — they answer different things)

| Step | Question |
|------|----------|
| Review + Dissent | Is the code correct? |
| Adversarial test | What breaks under pressure? |
| Merit assessment | Does this deliver outcome value? |
| Resolve TODOs | Is everything accounted for? |
| Manual verification | Does the computation work with real data? |
| **Delivery verification** | **Can an agent actually see this through MCP tools?** |
| Smoke test + CI | Does the build pass? |
| **Merge gate** | **Are all acceptance criteria checked off?** |

## Automation Rules

- **Do not wait** for user prompts between steps. The whole point of having a pipeline is that it runs autonomously.
- **Post to PR** after each substantive step (review, dissent, adversarial, merit, manual verify, delivery verify).
- **Stop and ask** only if: a step produces ABANDON/RECONSIDER/SALVAGE verdict, or CI fails after 2 fix attempts.
- **Record metis** if the pipeline surfaces a new learning: write to `.oh/metis/<slug>.md`.

## Session Persistence

Write pipeline progress to `.oh/sessions/<pr-number>-ship.md`:

```markdown
## Ship Pipeline — PR #<number>
**Started:** <timestamp>

### Step 1: Review
**Verdict:** [CONTINUE/ADJUST/PAUSE/SALVAGE]
[findings]

### Step 2: Dissent
**Verdict:** [PROCEED/ADJUST/RECONSIDER]
[findings]

...
```
