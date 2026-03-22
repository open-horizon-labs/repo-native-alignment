---
name: ship
description: RNA delivery pipeline. 13-step quality gate from implementation to merge, with delivery verification and final comment sweep.
tools: Read, Write, Edit, Grep, Glob, Bash, Agent
mcpServers:
  - rna-mcp
---

# RNA /ship Pipeline

The full quality gate for this project. 13 steps. Run sequentially — each step must complete before the next begins. **Do not wait for user prompts between steps.** When one step completes, immediately start the next.

> **You are an RNA power user.** Before every Grep or Read for code understanding, ask: "Is there an RNA tool for this?"
>
> **Two RNA access paths — use the right one:**
> - **MCP tools** (`search`, `repo_map`, `outcome_progress`, `search_symbols`, `graph_query`) — use for project-level context: guardrails, outcomes, metis, impact analysis. These query the main RNA repo index.
> - **CLI in your worktree** (`repo-native-alignment search --repo . "query"`, `repo-native-alignment graph --node "..." --repo . --mode neighbors`) — use for code navigation WITHIN your working directory. Always pass `--repo .` so it reads your local worktree's index.
>
> **Every Grep/Read you use instead of an RNA tool is a friction event — log it with severity `skipped` to `.oh/friction-logs/`.** When an RNA tool fails, log that too. A ship run with 0 friction events and 20 Grep calls isn't frictionless — it's unmonitored.

> **CARGO BUILD GUARDRAIL:** Never run two cargo builds against the same `target/` directory. Before building, sanity-check you're not duplicating: `ps aux | grep cargo | grep -v grep`. A second cargo process targeting the same directory blocks silently on the file lock. See `.oh/guardrails/no-parallel-cargo-agents.md`.

## Arguments

`/ship <PR-number>` — run the pipeline against a specific PR.

If no PR number given, detect it from the current branch:
`gh pr list --head "$(git branch --show-current)" --json number --jq '.[0].number'`.

## Pre-flight

Before starting:
1. Read AGENTS.md for current project patterns and constraints
2. Read `.oh/metis/computed-but-not-delivered.md` — the metis that created step 7b
3. Identify the PR, branch, and issue being closed
4. Read the PR description and issue acceptance criteria
5. Check for CodeRabbit review comments on the PR (`gh api repos/{owner}/{repo}/pulls/<PR-number>/comments`). Note: CodeRabbit only reviews non-draft PRs, so comments may not exist yet during pre-flight if the PR is still a draft.

## The 12 Steps

### 1. /review

Check implementation against acceptance criteria, AGENTS.md patterns, and guardrails.

**How:** Invoke the `/review` skill (or apply its process directly):
- **List every acceptance criterion** from the linked issue. Check each one against the implementation. If any are unmet, they become fix items for step 3.
- Restate the original aim
- Check: still necessary? still aligned? still sufficient? mechanism clear? changes complete?
- Detect drift (scope, solution, goal)
- Verdict: Continue / Adjust / Pause / Salvage

**Use RNA tools:** `search(query, artifact_types=["guardrail"])` to check against relevant guardrails.

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

**Use RNA tools:** `search("risks constraints", artifact_types=["guardrail", "metis"])` to ground the dissent.

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

Address and plausibly fix ALL findings from review, dissent, AND CodeRabbit. No deferred items.

**Sources to check:**
- Step 1 review findings
- Step 2 dissent findings — **every Hidden Assumption must be explicitly resolved:**
  - If the assumption is valid and acceptable: document WHY it's acceptable (scope decision, not an oversight)
  - If the assumption is wrong or risky: FIX IT before proceeding
  - "No risk" or "acceptable" verdicts from dissent are NOT automatic passes — they require explicit reasoning in the fix step
  - **Scope limitations are NOT acceptable when the issue promises broader coverage.** If the feature is supposed to work for all languages, "we only implemented 2 languages" is a blocker, not an assumption.
- **CodeRabbit PR review** — read all CodeRabbit comments with `gh pr view <PR> --comments` or `gh api repos/{owner}/{repo}/pulls/<PR>/comments`. CodeRabbit posts automated code review comments on every push. Treat these the same as review/dissent findings: fix, or explicitly mark N/A with reasoning.

If nothing to fix across all three sources, skip. Otherwise commit with descriptive messages.

### 3b. Mark PR ready for review

After fixing all findings, mark the PR as ready for review. This triggers smoke tests and CodeRabbit review in CI (both are gated behind `draft == false`).

```bash
gh pr ready <PR>
```

Wait briefly for CodeRabbit to start its review, then continue with the remaining steps. CodeRabbit findings will be addressed in step 6 (Resolve TODOs) if any arrive during the pipeline run.

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

**Performance gate (required for any PR that adds or modifies a post-extraction pass):**
Run a full scan of the RNA repo before and after and compare times:
```bash
time repo-native-alignment scan --repo . --full
```
If scan time increases by more than 10%, the pass must be optimized before merging. Do NOT declare "done" without this check. Common failure patterns:
- O(nodes × patterns) loops — must pre-index and use O(1) HashSet lookups
- String search inside a loop — extract candidates in one body pass, then check set membership
- Not gated by framework detection — passes that scan all nodes when they only apply to specific frameworks

**Issue hygiene:**
- If a feature doesn't work after merge → reopen the ORIGINAL issue, do NOT create a new one
- If a pass causes a perf regression → fix it in a follow-up PR, do NOT file a new issue and move on

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

- [ ] **Persist:** If new metadata on Node, is there a typed Arrow column in every relevant schema constructor (`symbols_schema()` and `symbols_schema_with_vector()` in store.rs)?
- [ ] **Write path:** Is the metadata written to the Arrow batch in BOTH construction sites in server.rs (initial + upsert)?
- [ ] **Read path:** Is the metadata read back from Arrow into Node.metadata during load?
- [ ] **Render:** Does the value appear in ALL relevant MCP tool outputs?
  - `search_symbols` formatting
  - `graph_query` / `format_neighbor_nodes` formatting
  - `search` code results formatting
- [ ] **End-to-end (MCP server path):** Start the MCP server and verify through the MCP protocol — not just CLI. Run `.github/scripts/mcp-smoke.mjs ./target/release/repo-native-alignment .github/fixtures/smoke` and verify the specific feature being tested. If the feature isn't covered by the smoke script, add an assertion to it or test manually with `npx @modelcontextprotocol/inspector`. **Unit tests and CLI scans do not substitute for this — the MCP server has a different code path and has regressed multiple times when only the CLI was tested.**

**If the PR doesn't add agent-visible data, mark this step N/A.**

**Post checklist results as PR comment:**
```bash
gh pr comment <PR> --body "$(cat <<'EOF'
## Ship Step 7b: Delivery Verification
- [x/blank] Persist: Arrow column in `symbols_schema()` (store.rs)
- [x/blank] Persist: Arrow column in `symbols_schema_with_vector()` (store.rs)
- [x/blank] Write path: initial batch construction (server.rs)
- [x/blank] Write path: upsert batch construction (server.rs)
- [x/blank] Read path: Arrow → Node.metadata during load
- [x/blank] Render: `search_symbols` formatting
- [x/blank] Render: `graph_query` / `format_neighbor_nodes` formatting
- [x/blank] Render: `search` code results formatting
- [x/blank] End-to-end: value visible in tool output after `cargo install --path .` + restart + rescan
- [x/blank] MCP server path: verified via `mcp-smoke.mjs` or `@modelcontextprotocol/inspector` (not just CLI)
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

### 10b. Final comment sweep

**Pre-merge gate: verify ALL PR comments are addressed.**

External reviewers (CodeRabbit, humans) post findings after step 3b (mark ready). The fix step (3) only catches findings that existed *before* marking ready. This step catches everything that arrived since.

**How:**
1. Fetch all PR comments:
   ```bash
   gh api repos/{owner}/{repo}/pulls/<PR>/comments --paginate
   gh api repos/{owner}/{repo}/issues/<PR>/comments --paginate
   ```
2. For each comment from a non-ship-agent source (CodeRabbit, humans, review skill):
   - Is there a commit that addresses it? → OK
   - Is there an explicit reply explaining why it's N/A? → OK
   - Neither? → **Fix it now** or explicitly mark N/A with reasoning
3. If any fixes were made, push and re-run step 9 (smoke test) and step 10 (CI green).

**Severity rules:**
- **Critical/Major findings** — must be fixed, no exceptions
- **Minor findings** — fix if straightforward, mark N/A with reasoning if not
- **Suggestions/nitpicks** — fix or explicitly acknowledge

**Post results as PR comment:**
```bash
gh pr comment <PR> --body "$(cat <<'EOF'
## Ship Step 10b: Final Comment Sweep
**External comments reviewed:** N
**Unaddressed findings fixed:** N
**Marked N/A with reasoning:** N

[details if any fixes were made]
EOF
)"
```

### 11. Merge

**Pre-merge gate: acceptance criteria.**

Before merging, re-read the linked issue's acceptance criteria. Every checkbox must be checked off — either verified done or explicitly filed as a follow-up issue with a link. If any criterion is unmet and not deferred, **do not merge**. Go back to step 3 (fix).

```bash
# Check off acceptance criteria on the issue
CURRENT_BODY="$(gh issue view <ISSUE> --json body --jq .body)"
UPDATED_BODY="$(printf '%s' "$CURRENT_BODY" | sed 's/- \[ \]/- [x]/g')"
gh issue edit <ISSUE> --body "$UPDATED_BODY"
# Then merge
gh pr merge <PR-number> --squash --delete-branch
```

## Step Questions (don't collapse steps — they answer different things)

| Step | Question |
|------|----------|
| Review + Dissent | Is the code correct? |
| Mark ready | Trigger smoke tests + CodeRabbit review |
| Adversarial test | What breaks under pressure? |
| Merit assessment | Does this deliver outcome value? |
| Resolve TODOs | Is everything accounted for? |
| Manual verification | Does the computation work with real data? |
| **Delivery verification** | **Can an agent actually see this through MCP tools?** |
| Smoke test + CI | Does the build pass? |
| **Final comment sweep** | **Are ALL external review comments addressed?** |
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
