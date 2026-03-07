# Session: P2 — Review Findings & Fixes

## Aim
**Updated:** 2026-03-07

Address all findings from /review, /dissent, and CodeRabbit on PR #1. Make the prototype honest about what it does and close the gap between claims and implementation.

## Problem Statement
**Updated:** 2026-03-07

The prototype has the layers but not the connections. The `query` tool is four independent substring matches unioned together — it's `search_all`, not an intersection query. The session file claims a vertical slice but the implementation is four horizontal slices stacked. The `.oh/ as cache` model has no population mechanism. The tree-sitter data model supports structural queries but the search doesn't use them. Several security, correctness, and design issues need fixing.

## Findings

### From /review (Critical + Major)

1. **[FIXED] Path traversal in `write_metis`** — slug validation + canonicalization added
2. **Path traversal in `file_history`** — `path` arg not validated as relative/within repo
3. **`split_frontmatter` fails on `\r\n`** — searches `"\n---"` only
4. **`extract_name` for trait impls** — `impl Display for Foo` named `Foo` not `Display for Foo`
5. **[FIXED] `query_all` swallows errors silently** — now logs warnings
6. **[FIXED] `.mcp.json` startup race** — now uses stdio transport

### From /review (Minor)

7. **`search_code` matches full function bodies** — common words match everything
8. **`oh_get_context` returns ALL unfiltered** — will blow context windows on real repos
9. **[FIXED] Unused deps** — tree-sitter-python, tree-sitter-typescript, axum, tower, tower-http removed
10. **[FIXED] HashMap non-determinism** — switched to BTreeMap
11. **`node_modules`, `vendor` not excluded** — directory walk only skips `.git/` and `target/`
12. **No round-trip test for `write_metis`**

### From /review (Missing functionality)

13. **[FIXED] No stdio transport** — added as default
14. **No `oh_search` tool** — can't search .oh/ by kind
15. **No write tools for outcomes/signals/guardrails** — only `oh_record_metis`
16. **No delete/update tools**

### From /dissent

17. **`query` is grep with extra steps** — four independent substring matches, no ranking, no join
18. **Rename `query` → `search_all`** — be honest about what it does
19. **No relational joins** — outcome doesn't know its commits, commits don't know their outcomes
20. **No commit tagging convention** — `[outcome:agent-alignment]` in commit messages would enable real joins
21. **`.oh/ as cache` has no ingestion** — no mechanism to populate from external sources
22. **Tree-sitter not used structurally** — search doesn't expose kind/scope filters
23. **Performance** — every tool call re-walks/re-parses everything from scratch
24. **Cold-start problem** — nobody creates `.oh/` files manually before seeing value

### From CodeRabbit
(Pending — will add when review arrives)

---

## Solution Space
**Updated:** 2026-03-07

**Selected:** Bug fixes + commit tagging convention + first real intersection query (`outcome_progress`)
**Level:** Reframe

**Rationale:** The dissent nailed it: the highest-leverage next step is making layers reference each other. Two lightweight mechanisms enable the first real cross-layer join:
1. Outcomes declare file patterns (`files: ["src/server.rs", "src/oh/**"]`)
2. Commits optionally tag outcomes (`[outcome:agent-alignment]`)

These links power `outcome_progress` — the first query that *joins* layers instead of unioning keyword matches.

**Accepted trade-offs:**
- File patterns in frontmatter are manually maintained
- Commit tagging is a convention, not enforced
- `outcome_progress` still re-parses on every call (no index yet)

### Implementation Checklist

#### Bug fixes
- [ ] Validate `file_history` path is relative and within repo
- [ ] Handle `\r\n` in `split_frontmatter`
- [ ] Fix `extract_name` for trait impls: `impl Display for Foo` → `Display for Foo`
- [ ] `search_code`: match name + signature only, not full body
- [ ] `oh_get_context`: cap symbols/chunks (e.g., 50 each) with total count
- [ ] Respect `.gitignore` in directory walks
- [ ] Add `write_metis` round-trip test

#### Rename + honesty
- [ ] Rename `query` tool → `search_all`
- [ ] Update tool description to say "multi-source substring search"

#### Commit tagging convention
- [ ] Document `[outcome:X]` convention
- [ ] `git::search_commits` gains `search_by_outcome_tag(outcome_id)`

#### Outcome file patterns
- [ ] Outcomes frontmatter gains optional `files:` field (glob patterns)
- [ ] `oh::load_oh_artifacts` parses the `files` field

#### `outcome_progress` — the real intersection query
- [ ] New MCP tool: `outcome_progress(outcome_id: String)`
- [ ] Finds the outcome by ID from `.oh/outcomes/`
- [ ] Finds commits tagged `[outcome:{id}]`
- [ ] Finds commits touching files matching outcome's `files:` patterns
- [ ] For changed files, finds code symbols defined there
- [ ] Returns: outcome summary → commit timeline → changed symbols → related markdown
- [ ] **This is the tool that proves the thesis**

#### Structural code search
- [ ] `search_code` gains optional `kind` filter
- [ ] `search_code` gains optional `file` glob filter
