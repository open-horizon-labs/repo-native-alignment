# Solution Space: Multi-Root Workspace Scanning

**Updated:** 2026-03-07
**Issue:** #12 — Multi-root: workspace-wide scanning with per-root config and sharded storage
**Outcome:** agent-alignment

## Problem

**The problem we are solving is:** The RNA server currently operates on a single `--repo` directory. Users work across multiple directories (project repos, zettelkasten, downloads, notes) and agents cannot discover context across these boundaries.

**The key constraint is:** Each root has fundamentally different characteristics (git-aware vs not, code vs notes vs general files), so a single scanning strategy and exclude pattern does not work. Yet cross-root search must feel unified to the agent.

**Success looks like:** An agent working in project A can search and find relevant context from the zettelkasten, downloads, or another project. Adding a new root is a config change, not infrastructure work. Per-root management (rescan, remove, re-exclude) works independently.

---

## Dimension 1: Config Format and Location

### Option A: Extend `.mcp.json` with roots array

- **Approach:** Add a `roots` key alongside existing `mcpServers` in each project's `.mcp.json`.
- **Level:** Band-Aid
- **Trade-off:** `.mcp.json` is per-project, but roots span projects. User must duplicate root config in every project that wants cross-root search. The schema is also owned by the MCP spec, not by RNA.

### Option B: User-level config file (`~/.config/rna/roots.toml`)

- **Approach:** A single user-level config file declares all roots. The server reads this on startup regardless of which project invoked it. TOML for readability and simplicity.
- **Level:** Local Optimum
- **Trade-off:** Requires the user to know about and edit a separate config file. Clean separation of concerns. Standard XDG location.

### Option C: CLI args passed through `.mcp.json`

- **Approach:** Extend the server's `--repo` arg to accept multiple `--root` flags. The `.mcp.json` passes these as args: `["--root", "~/src/zettelkasten:notes", "--root", "~/Downloads:general"]`.
- **Level:** Band-Aid
- **Trade-off:** Config lives in `.mcp.json` args, which is fragile and hard to read. No preset system. But zero new config files.

### Option D: Hybrid — user-level config + per-project overrides via CLI

- **Approach:** `~/.config/rna/roots.toml` declares the global root set. The `--repo` arg (or a `--project-root` arg) adds the current project as an additional root. Per-project `.mcp.json` just points to the binary and its project root; the global config handles everything else.
- **Level:** Reframe
- **Trade-off:** Two config sources to reason about, but matches how the user actually thinks: "these are my global directories" + "this is the project I'm in right now."

### Evaluation

| Option | Solves problem? | Implementation cost | Maintenance burden | Second-order effects |
|--------|----------------|--------------------|--------------------|---------------------|
| A: .mcp.json | Partially — duplication across projects | Low | High — config drift across projects | Schema conflict with MCP spec |
| B: ~/.config/rna/ | Yes | Medium | Low | Clean, but user must discover it |
| C: CLI args | Partially — awkward for many roots | Low | Medium — brittle arg parsing | Hard to add presets/excludes |
| D: Hybrid | Yes | Medium | Low | Matches mental model; init command can scaffold |

### Recommendation

**Selected:** Option D — Hybrid (user-level config + per-project CLI)

**Rationale:** Roots are a user-level concern that spans projects. The zettelkasten and downloads don't belong to any single project. A user-level config is the natural location. But the current project root should still come from `.mcp.json` args (as it does today with `--repo`), because that IS per-project. The hybrid approach means:

- `~/.config/rna/roots.toml` — declares persistent roots (zettelkasten, downloads, notes dirs)
- `--repo <path>` in `.mcp.json` — declares the current project root (as today)
- The server merges both at startup

**Accepted trade-offs:**
- Two config sources to document and debug
- Need an `rna roots add <path>` CLI command to make config easy
- The user-level config file must be created; `oh_init` or first run can scaffold it

### Config Format

```toml
# ~/.config/rna/roots.toml

[defaults]
# Default exclude patterns applied to all roots unless overridden
excludes = [".DS_Store", "Thumbs.db", ".Spotlight-V100/"]

[[roots]]
path = "~/src/zettelkasten"
type = "notes"
git_aware = true
# notes type preset: minimal excludes, index all markdown

[[roots]]
path = "~/Downloads"
type = "general"
excludes = ["*.iso", "*.dmg", "*.pkg", "*.zip.part"]
# general type preset: skip large binaries, index PDFs/docs

[[roots]]
path = "~/notes"
type = "notes"

# Root type presets (built-in, overridable):
# - code-project: excludes node_modules/, .venv/, target/, build/, __pycache__/, .git/objects/
# - notes: minimal excludes, prioritize .md files
# - general: exclude large binaries, OS metadata; index docs, PDFs, images (metadata only)
# - custom: no preset, user specifies everything
```

The current project root (from `--repo`) is implicitly added as type `code-project`.

---

## Dimension 2: Storage Sharding

### Option A: Single LanceDB database with root_id column

- **Approach:** One database at `~/.local/share/rna/lance/`. Every record has a `root_id` field. Cross-root queries are just queries without a root filter. Per-root cleanup is a delete-where.
- **Level:** Local Optimum
- **Trade-off:** Simpler querying (no union logic). But all roots share one index — a corrupt or huge root affects all. Cleanup requires scanning and deleting rows rather than dropping a directory.

### Option B: One LanceDB database per root

- **Approach:** Each root gets its own LanceDB instance at `~/.local/share/rna/lance/<root-hash>/`. Cross-root queries fan out across databases and merge results.
- **Level:** Local Optimum
- **Trade-off:** Clean isolation — remove a root by deleting its directory. But cross-root queries require fan-out logic, and relevance ranking across databases needs normalization.

### Option C: Single database, partitioned tables

- **Approach:** One LanceDB database, but separate tables per root (e.g., `zettelkasten_chunks`, `downloads_chunks`). Query one table or union across tables.
- **Level:** Reframe
- **Trade-off:** LanceDB tables are the natural unit of management. Drop a table to remove a root. Union queries across tables are straightforward in LanceDB. Embeddings stay comparable because they use the same model. Storage stays in one location for backup/management.

### Evaluation

| Option | Solves problem? | Implementation cost | Maintenance burden | Second-order effects |
|--------|----------------|--------------------|--------------------|---------------------|
| A: Single DB + column | Yes | Low | Medium — cleanup is row-level | Large roots can bloat shared index |
| B: Separate DBs | Yes | High | Low — drop directory to clean | Fan-out query complexity; score normalization |
| C: Partitioned tables | Yes | Medium | Low — drop table to clean | Natural LanceDB pattern; union queries simple |

### Recommendation

**Selected:** Option C — Single database, partitioned tables

**Rationale:** LanceDB tables are lightweight and independently manageable. This gives the isolation benefits of separate databases (drop a table to remove a root) without the fan-out complexity. Cross-root search unions across tables in one database connection, so embedding similarity scores are directly comparable. Storage location: `~/.local/share/rna/lance/` (XDG data directory).

**Table naming:** `{root_slug}_{content_type}` (e.g., `zettelkasten_chunks`, `downloads_chunks`, `project_x_symbols`).

**Accepted trade-offs:**
- All roots share one DB file — but LanceDB handles this well with columnar storage
- Table proliferation if many roots + content types — manageable with a registry table
- Need a metadata/registry table tracking root configs, last scan time, table names

---

## Dimension 3: Cross-Root Search

### Option A: Extend existing tools with root filter

- **Approach:** Add an optional `root` parameter to `oh_search_context`, `search_markdown`, `search_code`. Default: search all roots. If specified, restrict to one root.
- **Level:** Local Optimum
- **Trade-off:** Minimal API change. But does not help agents discover which roots exist.

### Option B: New cross-root search tool + root listing

- **Approach:** Add `rna_list_roots` (shows configured roots with stats) and `rna_search` (searches across all roots with root-aware result formatting). Keep existing tools for current-project-only queries.
- **Level:** Local Optimum
- **Trade-off:** More tools, but clearer semantics. Agents can discover roots, then search across them.

### Option C: Unified search with source attribution

- **Approach:** Extend `oh_search_context` to search all roots by default. Results include `root: zettelkasten` attribution. Add `rna_list_roots` for discovery. Do not create separate search tools — the existing ones transparently become multi-root.
- **Level:** Reframe
- **Trade-off:** Existing tools get broader scope (potentially noisier results). But agents do not need to learn new tools — the ones they already use become workspace-aware.

### Evaluation

| Option | Solves problem? | Implementation cost | Maintenance burden | Second-order effects |
|--------|----------------|--------------------|--------------------|---------------------|
| A: Filter param | Partially — no discovery | Low | Low | Agents must know root names |
| B: New tools | Yes | Medium | Medium — more tools to maintain | Tool sprawl |
| C: Transparent multi-root | Yes | Medium | Low — extends existing tools | May return noisy results across many roots |

### Recommendation

**Selected:** Option C — Unified search with source attribution

**Rationale:** The agent should not need to know about multi-root architecture. It searches for context and gets results. The results say where they came from. This is the principle of least surprise. Add one new tool (`rna_list_roots`) for root discovery and management, but keep search unified.

Implementation details:
- `oh_search_context` gains an optional `roots` parameter (list of root slugs to restrict). Default: all roots.
- Results include a `root` field in the markdown output (e.g., `[zettelkasten]` prefix).
- Current project root is boosted slightly in relevance ranking (1.2x multiplier) so local results appear first when scores are close.
- `rna_list_roots` returns: root slug, path, type, last scan time, item count.

**Accepted trade-offs:**
- Results from many roots could be noisy — mitigated by relevance ranking and optional root filter
- Slight boost for current project root is a heuristic — may need tuning

---

## Dimension 4: Discovery and JIT

### Option A: Strict — only declared roots are indexed

- **Approach:** If an agent references a path outside any declared root, the tool returns "path not in any configured root." User must add the root to config.
- **Level:** Band-Aid
- **Trade-off:** Predictable, no surprises. But breaks the flow — user has to stop and edit config.

### Option B: JIT file-level — scan referenced file on demand

- **Approach:** When an agent references a path outside declared roots, extract and index just that file. Do not add the containing directory as a root.
- **Level:** Local Optimum
- **Trade-off:** Useful for one-off references. Does not help with discovery of related files in the same directory.

### Option C: JIT suggest — scan the file, suggest adding the root

- **Approach:** When an agent references an undeclared path, scan that file (JIT) and return results. Also return a suggestion: "This path is not in a configured root. To index ~/some/dir, run `rna roots add ~/some/dir`." The agent or user can act on it.
- **Level:** Reframe
- **Trade-off:** Best of both worlds — immediate utility plus a path to permanent config. Does not auto-add roots (which could have unwanted side effects like indexing sensitive directories).

### Option D: Auto-discovery of common locations

- **Approach:** On first run, scan for common root locations (`~/src/*`, `~/Documents`, `~/Downloads`) and suggest them.
- **Level:** Redesign
- **Trade-off:** Feels magical, but risky. Could suggest directories the user does not want indexed. Better as an explicit `rna roots discover` command.

### Evaluation

| Option | Solves problem? | Implementation cost | Maintenance burden | Second-order effects |
|--------|----------------|--------------------|--------------------|---------------------|
| A: Strict | No — breaks flow | Low | Low | User friction |
| B: JIT file | Partially | Medium | Low | No directory-level discovery |
| C: JIT + suggest | Yes | Medium | Low | Gentle guidance without overreach |
| D: Auto-discover | Partially | Medium | Medium | Privacy/security concerns |

### Recommendation

**Selected:** Option C — JIT scan + suggest

**Rationale:** This respects the user's control over what gets indexed (no auto-adding roots to config) while still being useful in the moment. The suggestion provides a clear path to making the discovery permanent. Auto-discovery (Option D) is better as an explicit CLI command (`rna roots discover`) rather than automatic behavior.

**Accepted trade-offs:**
- JIT-scanned files are not persisted across sessions unless the root is added
- The suggestion mechanism requires the agent to relay the message to the user

---

## Summary Recommendation

| Dimension | Selected | Level |
|-----------|----------|-------|
| Config format | Hybrid: `~/.config/rna/roots.toml` + `--repo` CLI | Reframe |
| Storage sharding | Single LanceDB, partitioned tables per root | Reframe |
| Cross-root search | Transparent multi-root in existing tools + `rna_list_roots` | Reframe |
| Discovery / JIT | JIT scan + suggest adding root | Reframe |

### Architecture Sketch

```
~/.config/rna/roots.toml          <- declares persistent roots
.mcp.json --repo <path>          <- declares current project root

                 RNA Server
                     |
        ┌────────────┼────────────┐
        v            v            v
   project root   zettelkasten   downloads
   (code-project)   (notes)      (general)
        |            |            |
   tree-sitter   pulldown-cmark  metadata
   + markdown    heading chunks  extractor
   + git aware                   (PDF, etc.)
        |            |            |
        v            v            v
   ┌─────────────────────────────────┐
   │  LanceDB (~/.local/share/rna/) │
   │  ┌──────────┐ ┌─────────────┐  │
   │  │project_x │ │zettelkasten │  │
   │  │_chunks   │ │_chunks      │  │
   │  │_symbols  │ │             │  │
   │  └──────────┘ └─────────────┘  │
   │  ┌──────────┐ ┌─────────────┐  │
   │  │downloads │ │ _registry   │  │
   │  │_chunks   │ │ (metadata)  │  │
   │  └──────────┘ └─────────────┘  │
   └─────────────────────────────────┘
                     |
              MCP tool queries
         (union across root tables)
```

### Key Implementation Decisions

1. **Root slug derivation:** Last path component, slugified. `~/src/zettelkasten` becomes `zettelkasten`. Collisions resolved by appending a hash suffix.

2. **Registry table:** `_registry` table in LanceDB stores root metadata: slug, path, type, last_scan_at, config_hash. Used to detect config changes and trigger rescans.

3. **Extractor selection by root type:**
   - `code-project`: tree-sitter + pulldown-cmark + git history
   - `notes`: pulldown-cmark only (heading-delimited chunks, YAML frontmatter)
   - `general`: metadata extractor (file name, mtime, size, MIME type; PDF text extraction later)

4. **Change detection:**
   - Git roots: `git diff` for precision, mtime fallback
   - Non-git roots: mtime-based subtree skipping (proven at scale by fsPulse)
   - Per-root last-scan timestamp in registry; skip subtrees with mtime < last_scan

5. **Startup behavior:** Server reads `~/.config/rna/roots.toml` + `--repo` arg. Merges into root set. Checks registry for each root. Triggers background rescan for roots with changes. Does not block MCP tool availability on scan completion — stale results are better than no results.

6. **New CLI surface:**
   - `rna roots list` — show configured roots and scan status
   - `rna roots add <path> [--type notes|general|code-project]` — add to config
   - `rna roots remove <slug>` — remove from config + drop tables
   - `rna roots scan [slug]` — trigger rescan of one or all roots

7. **New MCP tool:** `rna_list_roots` — returns configured roots, types, last scan time, item counts. Enables agent to understand available context sources.

### What This Analysis Does Not Cover

- Embedding model selection and granularity (separate concern)
- LSP integration for code roots (future enhancement)
- Schema extraction from .proto / SQL files (future extractor)
- Sync between roots (e.g., zettelkasten linking to project code) — handled by cross-root search, not explicit sync

### Local Maximum Check

- Did I defend my first idea or actually explore? Explored four options per dimension, starting from the obvious (extend .mcp.json, single DB) and working up.
- Is there a higher-level approach I dismissed too quickly? A true redesign would be "make the server a workspace daemon that auto-discovers everything." I considered this (Option D in discovery) and concluded it's better as an explicit command. The user should control what gets indexed.
- Am I optimizing the wrong thing? The risk is over-engineering config when the real value is in cross-root search quality. The config format is intentionally simple (TOML, type presets) to keep the focus on search.
