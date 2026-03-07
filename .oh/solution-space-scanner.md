# Solution Space: Scanner Architecture

**Updated:** 2026-03-07
**Issue:** #7 — Scanner: mtime-based incremental file scanning with git optimization tier
**Outcome:** agent-alignment

## Problem

The RNA MCP server indexes only `.oh/` artifacts at startup. Agents need visibility into the full repo source tree and markdown to make informed decisions. The scanner must support incremental updates (mtime-based universal change detection, git diff as precision layer), JIT indexing for un-indexed files, and batch writes to LanceDB.

**Key Constraint:** The MCP server IS the daemon. No separate process. Scanner must integrate into the existing server lifecycle without blocking MCP tool availability.

**Success looks like:** Rescan with few changes < 5s. Agent can search code/markdown that was indexed at startup. Agent can reference an un-indexed file and get it extracted on demand.

## Candidates Considered

| Option | Level | Approach | Trade-off |
|--------|-------|----------|-----------|
| A | Band-Aid | Eager full walk at startup (current pattern, expanded) | Blocks MCP availability; no incremental; rescans everything |
| B | Local Optimum | Blocking startup scan with mtime skip + stored state | Startup delay scales with repo size; clean architecture |
| C | Reframe | Background scan with progressive availability + JIT fallback | MCP available immediately; complexity in concurrent index access |
| D | Redesign | Lazy/on-demand only — scan nothing at startup, index on first query | Zero startup cost; cold first queries; no proactive context |

### Option A: Eager Full Walk (Status Quo Extended)

Expand the current pattern: at startup, walk all `.rs` and `.md` files, extract everything, then start serving. This is what `extract_symbols` and `extract_markdown_chunks` already do — they walk the full tree every time they are called (per tool invocation, not even cached).

- **Solves stated problem:** Partially — indexes everything but no incremental, no state persistence
- **Implementation cost:** Low — extend existing `walk_repo_files` with more extensions
- **Maintenance burden:** Low — simple code
- **Second-order effects:** Current approach already re-walks on every `search_code` / `search_markdown` call. Extending this to more file types makes the per-call cost worse, not better. On large repos, each tool call would take seconds.
- **Does it enable future options:** No — no state means no incremental, no change detection

### Option B: Blocking Startup Scan with mtime State

At server startup, perform a full scan but use stored mtime state to skip unchanged subtrees. Persist scan state (directory mtimes, last git commit SHA) to `.oh/.cache/scan-state.json`. On subsequent starts, compare directory mtime before descending — unchanged subtrees are skipped entirely.

Architecture:
```
startup:
  load scan-state from .oh/.cache/scan-state.json
  walk tree, comparing dir mtimes:
    unchanged dir → skip entire subtree
    changed dir → list files, compare file mtimes
      changed file → queue for extraction
      new file → queue for extraction
      deleted file → mark for removal
  run extractors on queued files (batched)
  batch-write results to LanceDB
  save updated scan-state
  start serving MCP tools
```

- **Solves stated problem:** Yes — incremental, mtime-based, git-optimized
- **Implementation cost:** Medium — new scanner module, state persistence, batch writes
- **Maintenance burden:** Low — straightforward state machine, no concurrency concerns
- **Second-order effects:** Startup blocked until scan completes. For a typical project repo (hundreds to low thousands of files), this is <2s. For large monorepos, could be 10-30s on first scan, <2s incremental.
- **Does it enable future options:** Yes — scan state enables JIT (check if file is in index), multi-root (per-root state), background rescan (can be moved to background later)

### Option C: Background Scan with Progressive Availability

Start the MCP server immediately. Spawn a background tokio task for scanning. Tools that need indexed data either wait for the scan to complete or return partial results. JIT indexing handles files referenced before the background scan reaches them.

Architecture:
```
startup:
  start MCP server (immediately available)
  spawn background task:
    load scan-state
    walk tree with mtime skip
    extract changed files
    batch-write to LanceDB
    update scan-state

tool call arrives:
  if index ready → query index
  if index building →
    option 1: wait for completion (with timeout)
    option 2: return partial results + "indexing in progress"
    option 3: JIT-extract the specific file/query target

JIT path:
  agent references file not in index →
    extract immediately, write to index, return result
```

- **Solves stated problem:** Yes — all requirements met
- **Implementation cost:** High — concurrent index access (RwLock on LanceDB table or atomic table swap), progress tracking, partial-result handling
- **Maintenance burden:** Medium — concurrency bugs, race conditions between background scan and JIT writes, need to handle "table being rebuilt" states
- **Second-order effects:** MCP available instantly (good for agent experience). But concurrent writes to LanceDB require care — LanceDB supports concurrent readers but writers need coordination. The `OnceCell` pattern used for `embed_index` already handles lazy init, but background + JIT creates two writers.
- **Does it enable future options:** Yes — same as B, plus the background pattern enables periodic rescan during long sessions

### Option D: Lazy/On-Demand Only

Index nothing at startup. When an agent calls `search_code`, `search_markdown`, or any tool that needs file data, scan and extract on demand. Cache results in LanceDB for subsequent calls.

- **Solves stated problem:** Partially — no proactive indexing means first queries are slow
- **Implementation cost:** Medium — need per-query scan scope, caching layer
- **Maintenance burden:** Low — simple request/response model
- **Second-order effects:** First `search_code` call triggers full code scan (seconds of latency). `oh_get_context` would be very slow on first call. Agents typically call context tools early in a session, so the cold-start penalty hits at the worst time.
- **Does it enable future options:** Limited — no proactive context means compound queries (outcome_progress) can't join across layers until each layer has been independently queried

## Evaluation Summary

| Criterion | A (Eager) | B (Blocking) | C (Background) | D (Lazy) |
|-----------|-----------|-------------|----------------|----------|
| Solves stated problem | Partially | Yes | Yes | Partially |
| Implementation cost | Low | Medium | High | Medium |
| Maintenance burden | Low | Low | Medium | Low |
| Startup latency | High (grows) | Medium (bounded by mtime skip) | None | None |
| First-query latency | None (already loaded) | None | Possible (if scan not done) | High |
| Incremental support | No | Yes | Yes | No (per-query) |
| JIT support | N/A | Easy to add | Built-in | Is the whole model |
| Concurrency complexity | None | None | Significant | Low |

## Recommendation

**Selected:** Option B — Blocking Startup Scan with mtime State
**Level:** Local Optimum (with clear path to Reframe)

### Rationale

1. **The MCP server starts when an agent session begins.** A 1-2 second startup scan is invisible — the agent is still loading its own context. Blocking startup is not a real UX penalty for typical repos.

2. **Simplicity wins.** Option C's concurrency complexity (background scan + JIT writes + partial results) is engineering effort that solves a problem we do not yet have. The fsPulse salvage metis explicitly warns: "4-phase scanner state machine is overbuilt for RNA."

3. **mtime skip makes blocking fast.** First scan of a 5,000-file repo: ~2-3 seconds. Subsequent scans with few changes: <1 second. The mtime subtree skip is the core optimization — it makes blocking viable.

4. **Clear upgrade path.** If blocking startup becomes a bottleneck (large monorepos, multi-root), the scanner module can be moved to a background task without changing the scanner internals. The scan logic is the same; only the calling context changes. This is the B-to-C upgrade, and it is a small diff.

5. **JIT is a separate concern.** JIT indexing (extract a file on demand when not yet indexed) works the same regardless of whether the initial scan is blocking or background. Add it as a check in tool handlers: "is this file in the index? no? extract it now."

### Why Not the Others

- **Option A:** No incremental support. The current pattern of re-walking on every tool call is already a scaling problem. Extending it makes things worse.
- **Option C:** Premature complexity. Concurrent LanceDB writes, partial-result handling, and progress tracking solve a problem we don't have yet. The fsPulse lesson is clear: simpler state machines win.
- **Option D:** Lazy-only means cold first queries at the worst time (session start). Agents call `oh_get_context` and `search_code` early. Proactive indexing serves the common case.

### Accepted Trade-offs

- Startup is blocked until scan completes. For repos > 10K files, first scan could take 5-10 seconds. Acceptable because subsequent scans are incremental (<1s).
- JIT path is a separate mechanism from the startup scan. Two code paths that write to the index. Manageable because both go through the same extractor + batch-write layer.
- No partial results during scan. Either the index is ready or it is not. Simple to reason about.

## Implementation Architecture

### New Module: `src/scanner.rs`

```
pub struct Scanner {
    repo_root: PathBuf,
    state: ScanState,
    excludes: Vec<String>,
}

pub struct ScanState {
    /// Per-directory mtime at last scan
    dir_mtimes: HashMap<PathBuf, SystemTime>,
    /// Per-file mtime at last scan
    file_mtimes: HashMap<PathBuf, SystemTime>,
    /// Last indexed git commit (if .git present)
    last_commit_sha: Option<String>,
    /// Timestamp of last scan
    last_scan: SystemTime,
}

pub struct ScanResult {
    pub changed_files: Vec<PathBuf>,
    pub deleted_files: Vec<PathBuf>,
    pub scan_duration: Duration,
}
```

### Scan Flow

1. **Load state** from `.oh/.cache/scan-state.json` (or start fresh if absent)
2. **Git optimization** (if `.git` present):
   - If `last_commit_sha` set, run `git diff --name-only <sha>..HEAD` to get precise changed file list
   - Use `.gitignore` via git2 for exclude filtering (already in `walk.rs`)
   - If git diff succeeds, skip the mtime walk for those files — extract directly
3. **mtime walk** (universal fallback, also catches untracked files in git repos):
   - For each directory, compare mtime to stored state
   - Unchanged directory → skip entire subtree
   - Changed directory → enumerate files, compare file mtimes
   - Collect changed/new/deleted file lists
4. **Extract** changed files through appropriate extractors (by extension/type)
5. **Batch write** results to LanceDB (appender pattern, ~5000 batch size)
6. **Save state** — updated mtimes + current HEAD sha

### Integration with Existing Code

- **`walk.rs`** — keep for backward compat, but scanner replaces it for indexed queries. The scanner walks with mtime awareness; `walk.rs` walks without.
- **`code/mod.rs`** — `parse_rust_file` becomes an extractor called by the scanner, not a self-walking module. `extract_symbols` is still available for non-indexed paths.
- **`markdown/mod.rs`** — same pattern. `parse_markdown_file` is the extractor; the scanner drives it.
- **`embed.rs`** — `EmbeddingIndex` gains a method to ingest scanner output (batched file extractions) instead of only `.oh/` artifacts + commits.
- **`server.rs`** — `RnaHandler` gains a `scanner: OnceCell<Scanner>` or runs scan in constructor. Tool handlers for `search_code`, `search_markdown` query the LanceDB index instead of re-walking.

### State Persistence

- Location: `.oh/.cache/scan-state.json`
- Format: JSON (human-readable, easy to debug/reset)
- Contains: dir_mtimes, file_mtimes, last_commit_sha, last_scan timestamp
- Reset: delete the file to force full rescan

### Default Excludes

```
node_modules/
.venv/
target/
build/
__pycache__/
.git/objects/
dist/
.build/
vendor/
*.pyc
*.o
*.so
*.dylib
.DS_Store
```

### JIT Path (Additive)

When a tool handler needs a file not in the index:
1. Check if file path exists in LanceDB index
2. If not: extract immediately using appropriate extractor
3. Write to LanceDB (single-record write, not batched)
4. Return result

This is independent of the startup scan and can be added incrementally.

### LanceDB Schema Extension

Current `artifacts` table holds .oh/ artifacts + commits. The scanner needs additional tables:

- `code_symbols` — function/struct/trait definitions with file path, line range, signature
- `markdown_sections` — heading-delimited chunks with file path, heading hierarchy
- `file_index` — metadata for all indexed files (path, mtime, size, extractor used, last_indexed)

The `file_index` table enables JIT checks: "is this file already indexed?"

## Escalation Check

- Did I defend my first idea? No — I started leaning toward C (background) because it sounds more sophisticated. The fsPulse salvage explicitly warns against overbuilding, which redirected me to B.
- Is there a higher-level approach I dismissed too quickly? Option D (lazy-only) is philosophically interesting but fails the "agents need context early" use case. The proactive scan serves the common case.
- Am I optimizing the wrong thing? The real optimization is mtime subtree skipping, which works equally well in B or C. The startup-blocking vs background question is secondary to getting the mtime skip right.

## Next Steps

1. Implement `src/scanner.rs` with `ScanState` persistence and mtime-based walk
2. Add git optimization tier (git diff narrowing when .git present)
3. Wire scanner into `RnaHandler` startup (blocking, before MCP tools available)
4. Migrate `search_code` and `search_markdown` to query LanceDB index instead of re-walking
5. Add JIT extraction in tool handlers for un-indexed files
6. Add batch write to LanceDB with ~5000 batch size
