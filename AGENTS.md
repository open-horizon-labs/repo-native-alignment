# Open Horizons Framework

**The shift:** Action is cheap. Knowing what to do is scarce. We don't build features, we build capabilities.

**The sequence:** aim → problem-space → problem-statement → solution-space → execute → ship

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
| Recording learnings/signals | `oh_record(type, slug, ...)` |
| Searching git history | `git_history(query)` or `git_history(file)` |

**9 Tools (consolidated from 20+):**
1. `oh_get_context` -- read all business context (outcomes, signals, guardrails, metis)
2. `oh_search_context` -- semantic search (.oh/ artifacts, optionally code + markdown)
3. `oh_record` -- write any business artifact (metis, signal, guardrail, outcome update)
4. `oh_init` -- scaffold .oh/ directory
5. `outcome_progress` -- structural join for outcome tracking
6. `search_symbols` -- graph-aware code symbol search
7. `graph_query` -- graph traversal (neighbors, impact, reachable)
8. `git_history` -- commit search + file history
9. `list_roots` -- workspace root management

**Workflow:**
- Before starting work: call `oh_get_context` (business context auto-injected on first tool call)
- Explore code: `search_symbols` -> `graph_query(mode: "neighbors")` -> `graph_query(mode: "impact")`
- After completing work: call `oh_record(type: "metis", ...)` with key learnings
- When checking progress: call `outcome_progress` with `agent-alignment`
- When discovering constraints: call `oh_record(type: "guardrail", ...)`
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
- Use `oh_record(type, slug, ...)` to close the feedback loop (metis, signal, guardrail, outcome)
- Structural joins (outcome_progress) over keyword search for the core use case
- Pluggable extractors: implement `Extractor` trait for new file types (Phase 1: sync). `Enricher` trait for background enrichment (Phase 2: async, e.g., LSP)
- Graph model: `Node` + `Edge` types with `ExtractionSource` provenance and `Confidence` levels
- Source-capable records: wrap in `SourceEnvelope` at the outbox seam for future FEED publishing
- BTreeMap for frontmatter (deterministic output)
- YAML frontmatter + markdown body for all `.oh/` artifacts
- Scanner excludes configurable via `.oh/config.toml`
- Use compiler-driven refactoring (add field, let `cargo check` find every construction site)
- `cargo install --path .` before `/mcp` reconnect (or restart Claude Code)

## Anti-Patterns to Avoid
- Don't search function bodies in code search (noise) — match name + signature only
- Don't call a union of four greps an "intersection query"
- Don't test MCP with curl alone — protocol negotiation differs from real clients
- Don't port fsPulse's 4-phase scanner wholesale — RNA needs simpler: detect changed → extract → index

## Decision Context
Solo developer. PRs get /review and /dissent before merge. "Done" = tests pass, MCP client connects, tools exercised through real usage. Session learnings recorded as metis via MCP tools.

## Key Modules
- `src/graph/` — unified graph model (types, LanceDB schemas, petgraph index)
- `src/scanner.rs` — incremental file scanner (mtime + git + configurable excludes)
- `src/extract/` — pluggable extractors (Extractor trait + Enricher trait)
- `src/extract/{rust,python,typescript,go,markdown}.rs` — language-specific extractors
- `src/server.rs` — MCP server (9 intent-based tools)
- `src/embed.rs` — semantic search (fastembed + LanceDB)
- `src/query.rs` — outcome_progress structural joins

## Next Up
- LSP enricher (#9): cross-file references, type resolution → `Calls`/`Implements` edges
- Schema extractors (#10): .proto, SQL migrations, OpenAPI
- Multi-root workspace (#12): scan ~/src/zettelkasten, ~/Downloads, multiple project repos
- PR merge extraction: walk git merge history → pr_merge graph nodes
