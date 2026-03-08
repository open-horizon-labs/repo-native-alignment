---
id: worktree-awareness-wiring-not-design
outcome: agent-alignment
title: Worktree awareness is a wiring problem, not a design problem — NodeId.root already exists
---

## What Happened

PR #46 designed worktree awareness for RNA (root-scoped IDs + auto-detect `.git/worktrees/`). During design, it was found that `NodeId.root` already exists in the data model. The field was designed in, but never wired to the actual worktree detection or used in ID scoping.

## The Pattern

When a missing capability seems to require a new design, first audit whether the design is already there but unwired. In this case:
- `NodeId.root: Option<PathBuf>` — exists
- Worktree detection via `.git/worktrees/` — needs to be wired
- Using `root` in node deduplication — needs to be wired

The implementation work is connecting existing pieces, not designing new ones.

## Implication for RNA

Worktree support doesn't require a new architecture. It requires:
1. Auto-detect worktree roots at startup (`.git/worktrees/` symlink resolution)
2. Thread the detected root through indexing
3. Use root in NodeId equality/hashing

This is a PR, not a design phase.

## Evidence Source

PR #46 design artifact, `NodeId.root` field discovery during worktree awareness investigation.
