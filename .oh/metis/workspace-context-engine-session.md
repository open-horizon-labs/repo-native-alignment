---
id: workspace-context-engine-session
outcome: agent-alignment
title: 'Workspace context engine: from salvage to 20 working MCP tools in one session'
---

## What Happened

Single session went from fsPulse salvage → aim → problem space → solution space → execute → ship:

1. Salvaged fsPulse fork patterns (reverse delta, mtime skip, batch writes)
2. Aimed: workspace-wide context awareness for agents
3. Problem space: harness problem (Can Boluk), runtime architecture, schema extraction, PR-level change tracking
4. Solution space: 4 PRs covering scanner, graph, extractors, multi-root
5. Executed in 3 waves: graph model + scanner → extractors → wiring to MCP server
6. Shipped: 20 working MCP tools, 97 tests, installed binary

## Key Learnings

1. **Parallel agent waves work.** Graph + scanner built simultaneously in worktrees, merged cleanly. Extractor agent merged both and built on top.

2. **The "all known files" bug was subtle.** Scanner returns changed files only. On warm start, 0 changes → empty graph. Fix: extract ALL known files on graph init, not just the delta.

3. **Worktree management is friction.** 7 worktrees accumulated, locked branches, confusing state. Need to clean up aggressively after merging.

4. **`/mcp` reconnect is unreliable on long sessions.** Restarting Claude Code works; mid-session reconnect fails silently. This is a Claude Code issue, not RNA.

5. **Scanner excludes must be configurable from day 1.** `.omp/`, `.claude/worktrees/` — every user has directories that shouldn't be scanned. `.oh/config.toml` was the right answer.

6. **The validate-before-building guardrail override was correct.** Tree-sitter multi-language was the blocker to workspace-wide value. Building it was the validation — the graph works on first use.

7. **PR-level change tracking > per-commit.** Commits are too granular for outcome alignment. PRs have semantic intent (title, description) and bounded scope.
