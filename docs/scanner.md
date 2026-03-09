# Scanner

The RNA scanner is incremental, event-driven, and worktree-aware.

## Behavior

- Rescans in <1s -- only changed files re-extracted and upserted (O(changed files) end-to-end, including LanceDB)
- Event-driven reindex -- triggers immediately on `git pull`, `git merge`, or branch checkout; 15-minute heartbeat is the fallback, not the trigger
- Git worktrees indexed automatically -- agents running parallel branches see their own in-progress symbols, not the stale main-branch index
- Self-healing cache -- schema changes trigger automatic rebuild; no manual cache deletion needed

## Configuration

```toml
# .oh/config.toml
[scanner]
exclude = [".omp/", "data/", "*.log"]   # added to defaults
include = ["vendor/"]                     # opt back into something excluded by default
```

Default excludes: `node_modules/`, `.venv/`, `target/`, `build/`, `__pycache__/`, `.git/`, `.claude/`, `.omp/`, `dist/`, `vendor/`, `.build/`, `.cache/`
