---
id: hardlinked-cargo-locks-block-builds
outcome: agent-alignment
discovered: 2026-03-22
pr: "#488"
---

# Hardlinked .cargo-lock Files Block Parallel Builds

## What Happened

`prep-worktree.sh` uses `cp -al` to hardlink the main `target/` directory into
a new worktree. This gives a warm build cache instantly. The script removed
`target/.cargo-lock` to prevent lock contention.

But cargo places `.cargo-lock` at MULTIPLE levels:
- `target/.cargo-lock`
- `target/debug/.cargo-lock`
- `target/release/.cargo-lock`
- `target/aarch64-apple-darwin/release/.cargo-lock`

The inner lock files were NOT removed, causing parallel cargo processes to
block with:
```
Blocking waiting for file lock on build directory
Blocking waiting for file lock on artifact directory
```

This blocked builds for 30+ minutes across multiple pipeline agents during the
#474 (rayon) development session.

## Fix

Changed `prep-worktree.sh` to use `find ... -name ".cargo-lock" -delete` to
remove ALL cargo lock hardlinks at any depth. Committed in `200997e`.

## Lesson

When hardlinking a directory tree, EVERY advisory lock file at every level must
be individually de-linked. A shallow `rm -f` at the top level is insufficient
for tools that create locks in subdirectories.

## Guard

`no-parallel-cargo-agents.md` guardrail was correct in principle but the
implementation had this gap. The guardrail now holds: after the fix, separate
worktrees truly have independent cargo locks.
