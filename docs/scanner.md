# Scanner

The RNA scanner is incremental, event-driven, and worktree-aware.

## Behavior

- Rescans in <1s -- only changed files re-extracted and upserted (O(changed files) end-to-end, including LanceDB)
- Event-driven reindex -- triggers immediately on `git pull`, `git merge`, or branch checkout; 15-minute heartbeat is the fallback, not the trigger
- Git worktrees indexed automatically -- agents running parallel branches see their own in-progress symbols, not the stale main-branch index
- Worktree skip -- worktrees with their own RNA cache (`.oh/.cache/`) are automatically skipped during parent repo scans to avoid double-indexing (#524)
- Self-healing cache -- schema changes trigger automatic rebuild; no manual cache deletion needed
- Dirty-slugs filtering -- incremental scans track which root slugs have changed files, skipping LSP enrichment for unchanged roots
- Content-addressed consumer cache -- per-consumer cache keys (blake3 hash of event payload + consumer version) mean only consumers whose input changed re-run
- Scoped enrichment controls -- scans can run all enrichment, extract-only, without LSP, or without embeddings while preserving capability state in the operation report
- Durable operation telemetry -- scan/enrichment runs record phases, outputs, capability readiness, degraded query notices, next steps, and related enrichment job IDs for `list_roots`/CLI rendering

## Configuration

```toml
# .oh/config.toml
[scanner]
exclude = [".omp/", "data/", "*.log"]   # added to defaults
include = ["vendor/"]                     # opt back into something excluded by default
```

Directory and file patterns containing `/` are relative to the repository root
and are matched component by component, so `generated/schema/` excludes only
that subtree. Within a scoped pattern `*` matches inside a single component and
never crosses `/`: `generated/schema*/` excludes `generated/schema-v2/` too,
`generated/*.json` excludes JSON files directly inside `generated/` but not in
its subdirectories, and `crates/*/schema.json` excludes one such file per crate.
Unqualified directory names such as `data/` match that component at any depth,
and unqualified filenames such as `config.schema.json` match at any depth.

Notes for users with `.gitignore` habits:

- A leading `/` or `./` is accepted and normalized away; it makes the pattern
  root-anchored. `/gen/` excludes only the top-level `gen/` directory, whereas
  the unqualified `gen/` excludes a `gen` component at any depth. Doubled
  slashes (`gen//schema/`) are collapsed.
- `**` is **not** supported. It is treated as `*` and matches exactly one path
  component, so `gen/**/x.json` matches `gen/a/x.json` but not `gen/x.json` or
  `gen/a/b/x.json`. The scanner logs a warning for patterns containing `**`.
- A pattern that starts with `*` and contains `/` (for example `*/schema.json`)
  is a scoped pattern and matches at exactly that depth; previously such a
  pattern fell through to suffix matching at any depth. Use `schema.json` for
  any-depth filename matching or `*.json` for any-depth suffix matching.
- A trailing `/` does not distinguish files from directories: `gen/schema/`
  also excludes a file whose path is exactly `gen/schema`.

Default excludes: `node_modules/`, `.venv/`, `target/`, `build/`, `__pycache__/`, `.git/`, `.claude/`, `.omp/`, `dist/`, `vendor/`, `.build/`, `.cache/`
