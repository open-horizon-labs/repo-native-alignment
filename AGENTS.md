# Open Horizons Framework

**The shift:** Action is cheap. Knowing what to do is scarce. We don't build features, we build capabilities.

**The sequence:** aim → problem-space → problem-statement → solution-space → **draft PR** → execute → ship

**Where to start (triggers):**
- Can't explain why you're building this → `/aim`
- Keep hitting the same blockers → `/problem-space`
- Solutions feel forced → `/problem-statement`
- About to start coding → `/solution-space`
- Work is drifting or reversing → `/salvage`

**Reflection skills (use anytime):**
- `/review` - Check alignment before committing
- `/dissent` - Seek contrary evidence before one-way doors
- `/salvage` - Extract learning, restart clean

## Draft PR (between solution-space and execute)

After `/solution-space` produces a recommendation, **create a draft PR before writing code**. The draft PR:
- Names the branch, states the problem, and links the issue
- Summarizes the chosen solution from solution-space analysis
- Becomes the home for all /ship pipeline comments (review, dissent, adversarial test, merit assessment, etc.)
- `/execute` writes code on this branch; `/ship` runs the quality gate on this PR

This ensures every piece of work has a PR home before implementation begins. No orphan branches, no post-hoc PR creation.

## /ship Definition (for this project)

`/ship` = the full quality gate before merge. In order:

1. **`/review`** — check implementation against acceptance criteria, AGENTS.md patterns, and open issues
2. **`/dissent`** — seek contrary evidence; post findings as PR comments
3. **Fix all** — address every review + dissent finding; no deferred items
4. **Adversarial test** — dissent-seeded tests that try to break the implementation (functional > smoke > unit); post findings to PR
5. **Merit assessment** — is this worth merging? Run real queries, compare before/after. Verdict: MERGE / MERGE WITH CAVEATS / ABANDON / NEEDS MORE WORK. Post to PR.
6. **Resolve all TODOs** — every TODO, caveat, and "needs more work" item on the PR must be either **fixed** or **explicitly marked as not applicable with reasoning**. No silent deferrals. If it doesn't apply, say so on the PR. If it's a follow-up, file an issue and link it.
7. **Manual verification** — run the actual feature with real data. Not unit tests — real queries, real files, real output. Post results to PR.
8. **README** — update for any new capability, changed behavior, or new flags
9. **Smoke test** — update `src/smoke.rs` to exercise the new code path; `cargo test` must pass
10. **Merge** — squash or merge PR into main; tag commit with `[outcome:agent-alignment]`

No step is optional. "Merge when green" is not ship — ship is merge when reviewed, dissented, tested adversarially, merit-assessed, TODOs resolved, manually verified, and documented.

Steps answer different questions — don't collapse them:
- Review/dissent: "Is the code correct?"
- Adversarial test: "What breaks under pressure?"
- Merit assessment: "Does this deliver outcome value?"
- Resolve TODOs: "Is everything accounted for?"
- Manual verification: "Does it actually work with real data?"

**Key insight:** Enter at the altitude you need. Climb back up when you drift.

## Repo-Native Alignment MCP

This project IS the RNA MCP server. When working here, use its own tools.

**IMPORTANT: Use MCP tools for code exploration, NOT grep/Read/Bash.**

| Instead of... | Use this MCP tool |
|---|---|
| `Grep` for symbol names | `search_symbols(query, kind, language, file)` |
| `Read` to trace function calls | `graph_query(node_id, mode: "neighbors")` |
| `Grep` for "who calls X" | `graph_query(node_id, mode: "impact")` |
| `Read` to find .oh/ artifacts | `oh_search_context(query)` |
| `Bash` with `grep -rn` | `search_symbols` or `oh_search_context` |
| Recording learnings/signals | Write to `.oh/metis/`, `.oh/signals/`, `.oh/guardrails/` (YAML frontmatter + markdown) |
| Searching git history | `git_history(query)` or `git_history(file)` |

**If RNA returns empty results — diagnose before falling back:**
- Empty `search_symbols` means the symbol isn't indexed OR the query is wrong — try a broader query, different `kind`, or no filters first
- Empty `oh_search_context` means the index hasn't built yet OR the query is too specific — try simpler terms
- Do NOT silently fall back to Grep/Read on empty RNA results — that defeats the purpose
- If the index is genuinely stale, say so explicitly rather than substituting file reads

**7 MCP Tools (read + query only):**
1. `oh_get_context` -- read all business context (outcomes, signals, guardrails, metis)
2. `oh_search_context` -- semantic search (.oh/ artifacts, optionally code + markdown)
3. `outcome_progress` -- structural join for outcome tracking
4. `search_symbols` -- graph-aware code symbol search
5. `graph_query` -- graph traversal (neighbors, impact, reachable)
6. `list_roots` -- workspace root management

**Writing business artifacts:** Write directly to `.oh/` using the Write tool. See `.oh/metis/`, `.oh/signals/`, `.oh/guardrails/` for frontmatter templates.

**Workflow:**
- Before starting work: call `oh_get_context` (business context auto-injected on first tool call)
- Explore code: `search_symbols` -> `graph_query(mode: "neighbors")` -> `graph_query(mode: "impact")`
- After completing work: write learnings to `.oh/metis/<slug>.md`
- When checking progress: call `outcome_progress` with `agent-alignment`
- When discovering constraints: write to `.oh/guardrails/<slug>.md`
- Tag commits with `[outcome:agent-alignment]`

---

# Project Context

## Purpose
MCP server with a workspace-wide context engine. Incrementally scans repos, extracts a multi-language code graph (symbols, topology, schemas, PR history), and makes business outcomes, code structure, markdown, and git history queryable as one system. Agents stay aligned to declared intent because that intent lives in the repo as structured, queryable artifacts.

## Current Aims
- **agent-alignment** (active): Agents scope work to declared outcomes without user re-prompting. Mechanism: 9 intent-based MCP tools + OH Skills integration + outcome_progress structural joins.
- **workspace-context-engine** (active): Agents see context across the full workspace. Mechanism: incremental scanner + pluggable extractors (tree-sitter, LSP, markdown, schema) + unified graph (LanceDB + petgraph).

## Key Constraints
- **repo-native** (hard): No external store. `.oh/` in the repo, git-versioned. `rm -rf .oh/` loses context but breaks nothing.
- **lightweight** (hard): Adding an outcome = writing a markdown file. If heavier than a CLAUDE.md section, adoption fails.
- **git-is-optimization-not-requirement** (hard): Scanner works on any directory. Git adds precision when `.git` present.
- **extractors-are-pluggable** (soft): Don't hardcode extraction strategy per file type. tree-sitter, LSP, schema, markdown are all pluggable extractors behind the same trait.
- **name-tools-honestly** (soft): Tool names describe current behavior, not aspirations.
- **test-with-real-mcp-client** (candidate): Test MCP changes with TypeScript SDK or Claude Code, not curl.

## Patterns to Follow
- `[outcome:X]` in commit messages to link work to outcomes
- Write to `.oh/` to close the feedback loop (metis, signal, guardrail, outcome) — see existing files for frontmatter format
- Structural joins (outcome_progress) over keyword search for the core use case
- Pluggable extractors: implement `Extractor` trait for new file types (Phase 1: sync). `Enricher` trait for background enrichment (Phase 2: async, e.g., LSP)
- Graph model: `Node` + `Edge` types with `ExtractionSource` provenance and `Confidence` levels
- Source-capable records: wrap in `SourceEnvelope` at the outbox seam for future FEED publishing
- BTreeMap for frontmatter (deterministic output)
- YAML frontmatter + markdown body for all `.oh/` artifacts
- Scanner excludes configurable via `.oh/config.toml`
- Use compiler-driven refactoring (add field, let `cargo check` find every construction site)
- `cargo install --path .` before `/mcp` reconnect (or restart Claude Code)
- Parallel worktree builds: `scripts/prep-worktree.sh <path> <branch>` creates a worktree with warm build cache (hardlinks `target/`). Set `CARGO_TARGET_DIR=$WORKTREE/target` before cargo commands. Enables genuinely parallel builds on M4 Max without cache thrashing.

## Anti-Patterns to Avoid
- Don't search function bodies in code search (noise) — match name + signature only
- Don't call a union of four greps an "intersection query"
- Don't test MCP with curl alone — protocol negotiation differs from real clients
- Don't port fsPulse's 4-phase scanner wholesale — RNA needs simpler: detect changed → extract → index

## Decision Context
Solo developer. PRs go through the full /ship pipeline (10 steps). "Done" = all TODOs resolved, manually verified with real data, tests pass, MCP client connects. Session learnings recorded as metis via MCP tools.

## Key Modules
- `src/graph/` — unified graph model (types, LanceDB schemas, petgraph index)
- `src/scanner.rs` — incremental file scanner (mtime + git + configurable excludes)
- `src/extract/` — pluggable extractors (Extractor trait + Enricher trait)
- `src/extract/{rust,python,typescript,go,markdown}.rs` — language-specific extractors
- `src/server.rs` — MCP server (9 intent-based tools)
- `src/embed.rs` — semantic search (fastembed + LanceDB)
- `src/query.rs` — outcome_progress structural joins

## Shipped Capabilities
- LSP enrichment: 252 `Calls` edges via rust-analyzer callHierarchy (pyright, tsserver, gopls, marksman registered)
- Schema extractors: .proto, SQL, OpenAPI
- Multi-root workspace: `~/.config/rna/roots.toml` + per-root scanning
- PR merge extraction: git merge history → graph nodes + outcome_progress integration
- Graph persistence: LanceDB cache at `.oh/.cache/lance/`, loads in <1s on restart
- Context injection: business context auto-delivered on first tool call
