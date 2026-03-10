#!/usr/bin/env bash
# Prepare a git worktree with a warm Cargo build cache.
#
# Usage: scripts/prep-worktree.sh <worktree-path> <branch>
#
# Creates a worktree at <worktree-path> on <branch>, then hardlinks the
# main repo's target/ directory into it. This gives the worktree a warm
# build cache so `cargo build` only recompiles the delta.
#
# The worktree gets its own CARGO_TARGET_DIR so parallel builds in
# multiple worktrees don't fight over the same target/ directory.
#
# Example:
#   scripts/prep-worktree.sh .claude/worktrees/my-feature feat/my-branch
#   cd .claude/worktrees/my-feature
#   export CARGO_TARGET_DIR=$PWD/target
#   cargo build  # incremental, seconds not minutes

set -euo pipefail

WORKTREE_PATH="${1:?Usage: prep-worktree.sh <worktree-path> <branch>}"
BRANCH="${2:?Usage: prep-worktree.sh <worktree-path> <branch>}"

REPO_ROOT="$(git rev-parse --show-toplevel)"

# Create the worktree
git worktree add "$WORKTREE_PATH" "$BRANCH"

# Warm the build cache via hardlinks (instant, no disk space cost).
# Falls back to regular copy if hardlinks aren't supported (cross-device).
if [ -d "$REPO_ROOT/target" ]; then
    echo "Warming build cache via hardlinks..."
    cp -al "$REPO_ROOT/target" "$WORKTREE_PATH/target" 2>/dev/null \
        || cp -a "$REPO_ROOT/target" "$WORKTREE_PATH/target"
    echo "Done. Set CARGO_TARGET_DIR=$WORKTREE_PATH/target before building."
else
    echo "No target/ directory to copy — cold build."
fi

echo ""
echo "Worktree ready at: $WORKTREE_PATH"
echo "To build:"
echo "  cd $WORKTREE_PATH"
echo "  export CARGO_TARGET_DIR=\$PWD/target"
echo "  cargo build"
