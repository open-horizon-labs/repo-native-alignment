#!/usr/bin/env bash
# Prepare a git worktree with warm Cargo build cache AND warm RNA scan cache.
#
# Usage: scripts/prep-worktree.sh <worktree-path> [branch]
#
# Creates a worktree at <worktree-path> (optionally on <branch>), then:
# 1. Hardlinks the main repo's target/ for warm Cargo builds
# 2. Copies the main repo's .oh/.cache/ for warm RNA scans
#
# The RNA scan cache copy means agents can immediately use:
#   repo-native-alignment search --repo . "query"
# ...without waiting for a full scan. The incremental scan on first use
# only re-extracts changed files (typically < 5s for a fresh worktree).
#
# Example:
#   scripts/prep-worktree.sh .claude/worktrees/my-feature feat/my-branch
#   cd .claude/worktrees/my-feature
#   export CARGO_TARGET_DIR=$PWD/target
#   cargo build                                    # warm, seconds not minutes
#   repo-native-alignment search --repo . "query"  # warm, instant

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

# Warm the RNA scan cache — copy main repo's .oh/.cache/ to worktree.
# Since worktrees share the codebase, the incremental scan on first use
# only re-extracts changed files (< 5s for a typical agent worktree).
# This lets agents immediately use: repo-native-alignment search --repo . "query"
MAIN_CACHE="$REPO_ROOT/.oh/.cache"
WORKTREE_CACHE="$WORKTREE_PATH/.oh/.cache"
if [ -d "$MAIN_CACHE/lance" ]; then
    echo "Copying RNA scan cache for warm search..."
    mkdir -p "$WORKTREE_PATH/.oh"
    cp -a "$MAIN_CACHE" "$WORKTREE_CACHE"
    # Clear scan-state so incremental scan re-checks changed files
    rm -f "$WORKTREE_CACHE/scan-state.json"
    echo "RNA scan cache ready. Run: repo-native-alignment scan --repo . (incremental, fast)"
else
    echo "No RNA scan cache found — agents will need to run: repo-native-alignment scan --repo . --full"
fi

echo ""
echo "Worktree ready at: $WORKTREE_PATH"
echo "To use:"
echo "  cd $WORKTREE_PATH"
echo "  export CARGO_TARGET_DIR=\$PWD/target"
echo "  cargo build                                    # warm build"
echo "  repo-native-alignment scan --repo .            # warm incremental scan (fast)"
echo "  repo-native-alignment search --repo . 'query'  # immediate search"
