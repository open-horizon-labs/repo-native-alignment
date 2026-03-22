# /release

Prepare and present a release decision package. Does NOT release automatically — presents findings to the human to decide.

> **You are an RNA power user.** Use RNA MCP tools (`search`, `repo_map`, `outcome_progress`, `search_symbols`, `graph_query`) for all repo exploration — checking outcomes, finding issues, inspecting merged PRs, scanning guardrails. Use the CLI (`repo-native-alignment search --repo . "query"`) for worktree-specific queries. Every Grep/Read instead of an RNA tool is a friction event.

**This skill is hardass. A SKIP is not "safe to ship with." A SKIP means the feature is not done. Not done = blocking.**

## Hard rules

1. **SKIP = BLOCKING.** Any skipped test for a feature that was planned for this release is a hard blocker. Do not say "safe to ship with" for skipped tests. Say "BLOCKED — N features not yet complete."

2. **Zero tolerance for "safe to ship with" unfinished work.** If it was queued and isn't merged, it's blocking. Period.

3. **The decision package must start with a GO / NO-GO determination.** Not "here are the options." A clear recommendation with justification.

4. **Only merged, tested, passing features count.** Anything queued but not merged does not count toward the release.

## What this skill does

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
- Is it for a feature that was in scope for this release? → **BLOCKING**
- Is it for a future feature explicitly deferred? → **NOT BLOCKING** (document why it's deferred)

There is no middle ground. Either it was in scope (blocking) or it wasn't (document the deferral decision explicitly).

### Step 4: Add missing feature tests

For each merged PR since last tag:
- Does the full test suite exercise it?
- If not, add a test to the suite file
- Re-run

### Step 4b: Sweep all PRs and issues for unaddressed feedback

**This step is BLOCKING. Unaddressed Critical/Major findings = NO-GO.**

For every PR merged since last tag, check CodeRabbit inline comments AND issue comments:

```bash
# Get all merged PRs since last tag
git log <last-tag>..HEAD --merges --oneline | grep -o '#[0-9]*' | sort -u | while read pr; do
  echo "=== PR $pr ==="
  # CodeRabbit inline comments
  gh api repos/{owner}/{repo}/pulls/${pr#\#}/comments --paginate 2>/dev/null | \
    python3 -c "
import json,sys
cs = [c for c in json.load(sys.stdin) if 'coderabbit' in c.get('user',{}).get('login','').lower()]
for c in cs:
    sev = '🔴CRITICAL' if '🔴' in c.get('body','') else ('🟠MAJOR' if '🟠' in c.get('body','') else '🟡MINOR')
    print(f'  [{sev}] {c.get(\"path\",\"\")}:{c.get(\"line\",\"\")}')
    print(f'    {c.get(\"body\",\"\")[:150]}')
" 2>/dev/null
  # Issue comments
  gh api repos/{owner}/{repo}/issues/${pr#\#}/comments --paginate 2>/dev/null | \
    python3 -c "
import json,sys
cs = [c for c in json.load(sys.stdin) if 'coderabbit' not in c.get('user',{}).get('login','').lower() and 'github-actions' not in c.get('user',{}).get('login','').lower()]
for c in cs[:3]:
    print(f'  [HUMAN] {c.get(\"user\",{}).get(\"login\",\"\")}')
    print(f'    {c.get(\"body\",\"\")[:150]}')
" 2>/dev/null
done
```

For each finding:
- CRITICAL/MAJOR: **fix before release or it's NO-GO**
- MINOR: fix if trivial, otherwise explicitly reply with N/A reasoning
- Human comments: acknowledge or address

### Step 5: GO / NO-GO determination

**Before writing any release notes, state clearly:**

```
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
