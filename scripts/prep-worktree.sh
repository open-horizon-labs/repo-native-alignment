#!/usr/bin/env bash
# Prepare a git worktree with warm Cargo build cache and fresh RNA scan.
#
# Usage: scripts/prep-worktree.sh <worktree-path> [branch]
#
# Creates a worktree at <worktree-path> (optionally on <branch>), then:
# 1. Hardlinks the main repo's target/ for warm Cargo builds (saves 3-5 min)
# 2. Runs a fresh RNA scan so agents can use RNA immediately
#
# Note: we do NOT copy the main repo's .oh/.cache/ — a fresh scan is faster
# (~18-30s) than copying a large cache directory (~90s for typical repos).
# The fresh scan also guarantees correct data for the worktree's code.
#
# Example:
#   scripts/prep-worktree.sh .claude/worktrees/my-feature feat/my-branch
#   cd .claude/worktrees/my-feature
#   export CARGO_TARGET_DIR=$PWD/target
#   cargo build                                    # warm, seconds not minutes
#   repo-native-alignment search --repo . "query"  # immediately usable

set -euo pipefail

WORKTREE_PATH="${1:?Usage: prep-worktree.sh <worktree-path> [branch]}"
BRANCH="${2:-}"

REPO_ROOT="$(git rev-parse --show-toplevel)"

# Create the worktree (with or without a specific branch)
if [ -n "$BRANCH" ]; then
    git worktree add "$WORKTREE_PATH" "$BRANCH" 2>/dev/null \
        || git worktree add -b "$BRANCH" "$WORKTREE_PATH"
else
    git worktree add "$WORKTREE_PATH"
fi

# Warm the build cache via hardlinks (instant, no disk space cost).
# Falls back to regular copy if hardlinks aren't supported (cross-device).
#
# IMPORTANT: After hardlinking, we remove cargo's lock file (.cargo-lock)
# so each worktree gets its own lock. Hardlinked locks cause all worktrees
# to serialize behind one cargo process — defeating parallel builds.
if [ -d "$REPO_ROOT/target" ]; then
    echo "Warming build cache via hardlinks..."
    cp -al "$REPO_ROOT/target" "$WORKTREE_PATH/target" 2>/dev/null \
        || cp -a "$REPO_ROOT/target" "$WORKTREE_PATH/target"
    # Break hardlinks on ALL cargo lock files so parallel builds don't fight.
    # Cargo places .cargo-lock at multiple levels: target/, target/debug/,
    # target/release/, target/<arch>/release/, etc. All must be removed.
    find "$WORKTREE_PATH/target" -name ".cargo-lock" -delete 2>/dev/null || true
    rm -f "$WORKTREE_PATH/target/.package-cache"
    echo "Done. Set CARGO_TARGET_DIR=$WORKTREE_PATH/target before building."
else
    echo "No target/ directory to copy — cold build."
fi

# Warm the RNA scan cache — copy main repo's .oh/.cache/ then run incremental.
# Copy is fast (<5s for typical cache). Incremental scan picks up only changed
# files (~5-10s). Total: ~15s vs ~30-60s for a full cold scan.
MAIN_CACHE="$REPO_ROOT/.oh/.cache"
WORKTREE_CACHE="$WORKTREE_PATH/.oh/.cache"
if [ -d "$MAIN_CACHE/lance" ]; then
    echo "Copying RNA scan cache..."
    mkdir -p "$WORKTREE_PATH/.oh"
    cp -a "$MAIN_CACHE" "$WORKTREE_CACHE"
    rm -f "$WORKTREE_CACHE/scan-state.json"  # force incremental re-check
    echo "Running incremental scan (picks up changed files only)..."
    if (cd "$WORKTREE_PATH" && repo-native-alignment scan --repo . 2>&1 | tail -2); then
        echo "RNA ready. Use: repo-native-alignment search --repo . 'query'"
    else
        echo "Scan failed — run manually: repo-native-alignment scan --repo . --full"
    fi
else
    echo "No RNA cache found — running full scan..."
    (cd "$WORKTREE_PATH" && repo-native-alignment scan --repo . --full 2>&1 | tail -2) || true
fi

echo ""
echo "Worktree ready at: $WORKTREE_PATH"
echo "To use:"
echo "  cd $WORKTREE_PATH"
echo "  export CARGO_TARGET_DIR=\$PWD/target"
echo "  cargo build                                    # warm build"
echo "  repo-native-alignment scan --repo .            # warm incremental scan (fast)"
echo "  repo-native-alignment search --repo . 'query'  # immediate search"
