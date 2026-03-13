---
id: no-parallel-cargo-agents
outcome: agent-alignment
severity: hard
statement: Never run two cargo builds against the same target directory. Each pipeline gets its own worktree with its own CARGO_TARGET_DIR.
---

## Rule

1. **One cargo build per target directory.** Before running `cargo build`, `cargo test`, or `cargo clippy`, sanity-check you're not duplicating a build against the same `target/`. If in doubt: `ps aux | grep cargo | grep -v grep`.
2. **Parallel pipelines are fine** — but each must use its own worktree with a separate `CARGO_TARGET_DIR` via `scripts/prep-worktree.sh`.
3. **Sequential /ship** — run /ship pipelines one at a time since they post to GitHub and need focused review.

## Rationale

Cargo uses an advisory file lock on `target/.cargo-lock`. A second cargo process targeting the same directory blocks silently — printing `Blocking waiting for file lock on build directory` — hanging indefinitely and wasting tokens.

With separate target directories (via worktrees), parallel builds are safe from lock contention. CPU/memory pressure on large codebases is the remaining concern but manageable for 2-3 concurrent pipelines.

## Evidence

- 2026-03-12: 5 parallel agents sharing target directories. ALL killed after extended build contention. Only 1 of 5 completed.
- 2026-03-13: Agent started duplicate `cargo build --release` in same repo while first was still running. Second build blocked on lock.

## What goes wrong without this

1. **Cargo lock contention** — parallel builds to same target serialize behind one lock, others hang indefinitely
2. **Build timeouts** — blocked builds hit Bash timeout (2 min), agents retry in loops
3. **Token waste** — agents burn thousands of tokens on timed-out or blocked builds
