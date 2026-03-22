---
name: worktree-setup
description: Set up and use RNA in a git worktree. Scan the index, verify it's live, establish correct tool usage, and clean up after merge.
tools: Read, Write, Edit, Grep, Glob, Bash
mcpServers:
  - rna-mcp
---

# /worktree-setup

Bootstrap RNA in a git worktree so code navigation is fast and correct from the first tool call.

Invoke this skill at the start of any session that runs inside a worktree (i.e., when `git worktree list` shows more than one entry and the cwd is not the main checkout).

> **You are an RNA power user.** Use RNA MCP tools (`search`, `repo_map`, `outcome_progress`, `graph_query`) for all code navigation — never Grep or Read for code understanding. Every Grep/Read instead of an RNA tool is a friction event. Log it with `/friction`.

---

## Quick setup (TL;DR)

```bash
# 1. Scan (fast — warm cache via prep-worktree.sh already ran)
repo-native-alignment scan --repo . 2>&1 | tail -2

# 2. Verify index is live
COUNT=$(repo-native-alignment search "" --repo . --limit 1 2>/dev/null | grep -o "[0-9]* symbols" | head -1)
echo "RNA: $COUNT indexed"
# If COUNT is 0 or missing → run full scan and investigate (see Step 2 below)

# 3. Navigate with RNA — never Grep/Read for code
mcp__rna-mcp__repo_map     # orientation
mcp__rna-mcp__search       # symbol / semantic search
```

---

## Full process

### Step 1: Scan the worktree

The worktree's `.oh/.cache/` was seeded from the main repo's cache by `scripts/prep-worktree.sh`. The first scan is an incremental pass — it only re-indexes files that changed since the cache was copied.

```bash
repo-native-alignment scan --repo . 2>&1 | tail -2
```

Expected output (warm cache):
```
Scanned 312 files in 14s (12 changed, 300 cached)
Index ready.
```

If the script hasn't been run yet (cold start), run a full scan:
```bash
repo-native-alignment scan --repo . --full 2>&1 | tail -2
```

Full scans take 30–60 s for a typical Rust repo. Warm incremental scans take ~15 s.

### Step 2: Verify the index is live

```bash
COUNT=$(repo-native-alignment search "" --repo . --limit 1 2>/dev/null | grep -o "[0-9]* symbols" | head -1)
echo "RNA: $COUNT indexed"
```

**If COUNT is 0 or the command errors:**

1. Run a forced full scan:
   ```bash
   repo-native-alignment scan --repo . --full 2>&1
   ```
2. Verify the binary is on `$PATH`:
   ```bash
   which repo-native-alignment 2>/dev/null \
     || ls target/release/repo-native-alignment 2>/dev/null \
     || ls target/debug/repo-native-alignment 2>/dev/null
   ```
3. Check that `--repo .` resolves to the worktree root (not the main checkout).

Do not proceed to code navigation until the index reports a non-zero symbol count. An empty index means every RNA call returns nothing — agents silently fall back to Grep, and friction goes undetected.

### Step 3: Use RNA for ALL code navigation

Once the index is live, use these tools exclusively for code understanding:

**MCP (preferred — no subprocess overhead):**
```
mcp__rna-mcp__repo_map(root="/absolute/path/to/worktree")
mcp__rna-mcp__search(query="...", root="/absolute/path/to/worktree")
mcp__rna-mcp__search(query="...", root="/absolute/path/to/worktree", mode="neighbors", node="file:sym:kind")
mcp__rna-mcp__outcome_progress(outcome_id="...", root="/absolute/path/to/worktree")
```

**CLI (useful for ad-hoc exploration or when MCP isn't configured):**
```bash
repo-native-alignment search "query" --repo . --limit 5
repo-native-alignment search "query" --repo . --kind function --compact
repo-native-alignment graph --node "file:sym:kind" --repo . --mode neighbors
repo-native-alignment graph --node "file:sym:kind" --repo . --mode impact
```

**Never use Grep or Read for code understanding** — see the RNA tool quick reference in `.claude/skills/friction.md`.

The extractor contribution pattern (how RNA extracts framework-level edges) is documented in `docs/extractors.md`. Read it if you need to understand how a new extractor would be added, or when reviewing extraction-related code.

### Step 4: Work normally

Build, test, edit as usual. The worktree has its own `target/` (hardlinked from the main repo for warm builds) and its own `.oh/.cache/` (seeded from the main repo, then incremental).

```bash
export CARGO_TARGET_DIR=$PWD/target   # isolate builds — required per guardrail
cargo check --lib
cargo test
```

Re-scan after significant edits to keep the index current:
```bash
repo-native-alignment scan --repo . 2>&1 | tail -2
```

### Step 5: Clean up after merge

Once the PR is merged, remove the worktree from both the filesystem and git's tracking list:

```bash
# From the MAIN repo checkout (not the worktree itself)
git worktree remove /absolute/path/to/worktree --force
git worktree prune
```

Verify no stale entries remain:
```bash
git worktree list
```

---

## Friction logging

Every Grep or Read used for code understanding after the scan is live is a friction event. Log it.

The file: `.oh/friction-logs/<issue-or-context>.md`

```markdown
| Phase/Step | Tool | What happened | Workaround | Severity |
|------------|------|---------------|------------|----------|
| Step 3 | search (skipped) | Needed callers of build_node_id | Used Grep for "build_node_id" | skipped |
```

See `.claude/skills/friction.md` for severity levels and the full logging format.

---

## Reference

| Need | RNA tool | CLI equivalent |
|------|----------|----------------|
| Orientation | `mcp__rna-mcp__repo_map` | `repo-native-alignment repo-map --repo .` |
| Find by intent | `mcp__rna-mcp__search(query)` | `repo-native-alignment search "query" --repo .` |
| Find by name/kind | `mcp__rna-mcp__search(query, kind)` | `repo-native-alignment search "q" --repo . --kind function` |
| Trace dependencies | `mcp__rna-mcp__search(mode="neighbors")` | `repo-native-alignment graph --node ... --mode neighbors --repo .` |
| Blast radius | `mcp__rna-mcp__search(mode="impact")` | `repo-native-alignment graph --node ... --mode impact --repo .` |
| Outcome alignment | `mcp__rna-mcp__outcome_progress` | `repo-native-alignment outcome-progress <id> --repo .` |

**Always pass an absolute path** when using the MCP `root` parameter in a worktree — relative paths resolve against the MCP server's cwd, not the worktree.

---

## Known limitation: main repo re-indexes worktrees

If `repo-native-alignment scan` is run from the main checkout while an agent is active in a worktree, the main scan will walk into the worktree and re-index its files. This pollutes the main index with in-progress code and wastes scan time.

Tracked in issue #518: `with_worktrees()` should skip worktree directories that have their own `.oh/.cache/lance/`.

Until that fix lands, avoid running `repo-native-alignment scan` from the main checkout while worktrees are active with their own caches.

---

## Setup script reference

`scripts/prep-worktree.sh` automates Steps 1 and 2 above. Run it from the main repo checkout before switching into the worktree:

```bash
scripts/prep-worktree.sh .claude/worktrees/my-feature feat/my-branch
cd .claude/worktrees/my-feature
export CARGO_TARGET_DIR=$PWD/target
# Index is already warm — skip to Step 3
```

See the script itself for details on cache seeding, hardlink strategy, and fallback behavior.
