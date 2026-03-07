---
id: git-is-optimization-not-requirement
outcome: agent-alignment
severity: hard
statement: Git is an optimization layer for change detection, not a requirement. The scanner must work on arbitrary directories (~/Downloads, zettelkasten, non-git project dirs) using mtime-based scanning.
---

## Rationale

The salvage initially recommended "use git diff, not filesystem scanning." The user corrected: the scope is workspace-wide, not repo-only. ~/Downloads has no .git. Zettelkasten spans multiple repos and loose files.

## What Happened

fsPulse proved mtime-based subtree skipping works at scale (1.5M files in seconds). This is the universal change detection layer. Git adds precision when available (exact changed files, commit attribution, .gitignore as exclude list) but many valuable directories aren't git repos.

## Override Protocol

If a future use case is exclusively git-repo scoped, git-only mode is fine as an optimization. But the core scanner must not assume `.git` exists.
