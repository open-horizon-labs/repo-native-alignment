---
name: release
description: Prepare and present a release decision package. Runs test suite, sweeps PR feedback, assesses GO/NO-GO, writes outcome-oriented release notes.
tools: Read, Write, Edit, Grep, Glob, Bash, Agent
mcpServers:
  - rna-mcp
---

# /release

Prepare and present a release decision package. Does NOT release automatically — presents findings to the human to decide.

> **You are an RNA power user.** Use RNA MCP tools (`search`, `repo_map`, `outcome_progress`, `list_roots`) for all repo exploration — checking outcomes, finding issues, inspecting merged PRs, scanning guardrails. Use the CLI (`repo-native-alignment search --repo . "query"`) for worktree-specific queries. Every Grep/Read instead of an RNA tool is a friction event.

**This skill is hardass. A SKIP is not "safe to ship with." A SKIP means the feature is not done. Not done = blocking.**

## Hard rules

1. **SKIP = BLOCKING.** Any skipped test for a feature that was planned for this release is a hard blocker. Do not say "safe to ship with" for skipped tests. Say "BLOCKED — N features not yet complete."

2. **Zero tolerance for "safe to ship with" unfinished work.** If it was queued and isn't merged, it's blocking. Period.

3. **The decision package must start with a GO / NO-GO determination.** Not "here are the options." A clear recommendation with justification.

4. **Only merged, tested, passing features count.** Anything queued but not merged does not count toward the release.

## What this skill does

0. **Use RNA tools for code exploration** before drawing conclusions:
   - Inspect changed files and related symbols with RNA.
   - Use RNA evidence when determining scope, blockers, and smoke candidates.
   - Do not rely on assumptions when repository evidence is available.

1. **Run full test suite** (`scripts/test-suite.sh`)
2. **Add feature tests** for anything new since last tag not already in the suite
3. **Hard pass/fail assessment** — SKIP = blocking, FAIL = blocking, PASS = good
4. **GO / NO-GO determination** based on test results
5. **Salvage analysis** — what should be promoted to smoke regression
6. **Outcome-framed release notes** (before/after, not feature list)
7. **Present decision package** — human decides RELEASE / TWEAK / NOT

## Process

### Step 1: Establish baseline

```bash
git describe --tags --abbrev=0  # last release tag
git log <last-tag>..HEAD --oneline | wc -l  # commits since
```

### Step 2: Run full test suite

Run `bash scripts/test-suite.sh` (or with IC: `bash scripts/test-suite.sh $RNA_REPO $IC_REPO`).

### Step 3: Hard assessment of skips

For EVERY skipped test:
- Is it for a feature that was in scope for this release baseline/scope freeze? → **BLOCKING**
- Is it for a feature explicitly marked out-of-scope before scope freeze? → **NOT BLOCKING** (document deferral reference — link to issue or decision)

There is no middle ground. Either it was in scope at freeze (blocking) or it was explicitly deferred before freeze (document it). "We can ship without it" is not a deferral decision.

### Step 4: Add missing feature tests

For each merged PR since last tag:
- Does the full test suite exercise it?
- If not, add a test to the suite file
- Re-run

### Step 4b: Sweep all PRs and issues for unaddressed feedback

**This step is BLOCKING. Unaddressed Critical/Major findings = NO-GO.**

For every PR merged since last tag, check all inline review comments and issue comments. CodeRabbit is optional supplemental feedback; never trigger or wait for it, but include any comments it already posted:

```bash
set -euo pipefail

# Get all PRs merged to main since the last tag, including squash merges
LAST_TAG_EPOCH=$(git log -1 --format=%ct <last-tag>)
gh pr list --state merged --base main --limit 1000 --json number,mergedAt | \
  python3 -c "
import datetime,json,sys
since = int(sys.argv[1])
for pr in json.load(sys.stdin):
    merged = datetime.datetime.fromisoformat(pr['mergedAt'].replace('Z', '+00:00')).timestamp()
    if merged > since:
        print(pr['number'])
" "$LAST_TAG_EPOCH" | while read pr; do
  echo "=== PR #$pr ==="
  # All inline review comments, including optional CodeRabbit feedback
  gh api repos/{owner}/{repo}/pulls/${pr}/comments --paginate --slurp | \
    python3 -c "
import json,sys
pages = json.load(sys.stdin)
cs = [c for page in pages for c in page]
for c in cs:
    sev = '🔴CRITICAL' if '🔴' in c.get('body','') else ('🟠MAJOR' if '🟠' in c.get('body','') else '🟡MINOR')
    print(f'  [{sev}] {c.get(\"user\",{}).get(\"login\",\"\")} {c.get(\"path\",\"\")}:{c.get(\"line\",\"\")}')
    print(f'    {c.get(\"body\",\"\")}')
"
  # Issue comments
  gh api repos/{owner}/{repo}/issues/${pr}/comments --paginate --slurp | \
    python3 -c "
import json,sys
pages = json.load(sys.stdin)
cs = [c for page in pages for c in page if 'github-actions' not in c.get('user',{}).get('login','').lower()]
for c in cs:
    print(f'  [COMMENT] {c.get(\"user\",{}).get(\"login\",\"\")}')
    print(f'    {c.get(\"body\",\"\")}')
"
done
```

For each finding:
- CRITICAL/MAJOR: **fix before release or it's NO-GO**
- MINOR: fix if trivial, otherwise explicitly reply with N/A reasoning
- Human comments: acknowledge or address

### Step 5: GO / NO-GO determination

**Before writing any release notes, state clearly:**

```text
GO / NO-GO: [GO|NO-GO]

Reason: [one sentence]

Blockers:
- [list each blocker with issue number, or "none"]
```

If NO-GO: stop here. Do not write release notes. Tell the human what needs to land.

### Step 6: Salvage analysis (only if GO)

Use `/salvage` on the test suite:
- Which tests exercise the most critical paths?
- Which failures would be caught EARLIEST if added to smoke?
- Recommend max 5 new smoke test candidates

### Step 7: Release notes (only if GO)

Frame as OUTCOME changes, not feature list:

```markdown
## What changed for users/agents

### Before this release
- [pain point 1]: [what was hard or impossible]

### After this release
- [pain point 1]: [how it's now solved]

## Breaking changes
[schema version bump, slug format change, etc.]

## Issues addressed
[linked list]
```

### Step 8: Present decision package

**START with GO / NO-GO.**

Then show:
1. Test results (N passed / N failed / N skipped + blocking assessment)
2. If GO: smoke regression candidates, release notes, recommended version bump
3. If NO-GO: blockers list only — no release notes needed

**WAIT for human decision before doing anything.**

If RELEASE:
- Bump version in Cargo.toml if not already bumped
- Create release commit + tag
- Push tag (CI builds release artifacts)

If TWEAK:
- Address specific feedback, re-run from Step 2

If NOT:
- Note what needs to be fixed before next release attempt
