# Session: repo-native-alignment

## Aim
**Updated:** 2026-03-06

**Aim:** Agents working in a codebase stay aligned to declared business outcomes — not just code correctness — because the outcomes, constraints, and progress signals live *in the repo* as queryable, evolving artifacts that agents read from and write to during normal work.

**Current State:** Agents operate in a vacuum. CLAUDE.md tells them code conventions. Maybe there's a README with project goals. But there's no structured, queryable record of: what business outcome are we optimizing for? What signals tell us we're on track? What have we tried? What did we learn? Agents rediscover intent every session from scattered prose, or worse, from the user re-explaining it.

**Desired State:** An agent starting a session can query: "What are the active business outcomes for this project? What SLOs are defined? What's the current state of each? What constraints apply? What have we learned?" — and get structured answers from the repo itself. When work is completed, the agent (or harness) records what was done towards which outcome, with what signal. Over time, the repo accumulates *situated judgment* — metis — that compounds.

### Mechanism

**Change:** A harness (tooling + conventions + repo structure) that:
1. Stores **outcomes** (aims), **signals** (SLOs/feedback), **constraints** (guardrails), and **learnings** (metis) as structured artifacts in the repo
2. Makes them **queryable** by agents at execution time (not just readable prose — structured enough to filter, match, and reason over)
3. **Evolves** through normal work — agents contribute observations, humans curate and promote

**Hypothesis:** Agent reliability improves when alignment context is explicit, queryable, and co-located with the code — because the agent doesn't have to infer intent, it reads it. The same structure that makes *code* semantically queryable (CodeState) applies to *business context*: outcomes, signals, and constraints are objects, not strings.

**Assumptions:**
1. Markdown + structured frontmatter in the repo is sufficient as the storage format (agents already parse markdown natively; git provides versioning and history)
2. The harness is lightweight enough that people actually use it (if it's heavier than writing a CLAUDE.md section, adoption fails)
3. Agents can meaningfully reason over outcome/signal/constraint triples at execution time (not just "read the file" — actually scope decisions by them)
4. The value compounds — metis from early sessions genuinely improves later sessions

### Feedback

**Signal:** An agent, given a task, can answer "which outcome does this serve?" and "what constraints apply?" without the user explaining it. Measurable by: does the agent scope its work correctly without re-prompting?

**Timeframe:** Immediate per-session. Compound effect visible after 5-10 sessions of accumulated metis.

### Guardrails

- **Repo-native**: no external store, no platform dependency. If you `rm -rf .oh/`, you lose context but nothing breaks.
- **Lightweight**: adding an outcome is writing a markdown file, not configuring a system.
- **Git-versioned**: every change to outcomes/signals/constraints is a commit. History is free.
- **Agent-writable but human-curated**: agents can propose metis; humans promote it to guardrails.
- **Not a replacement for OH graph**: this is the *local, repo-embedded* slice of what OH does at the organizational level. They can sync.

---

## Problem Statement
**Updated:** 2026-03-06

**Current framing:** Agents lose alignment with business intent every session because intent lives in unstructured prose they can't reason over.

**Reframed as:** Agents can't answer "what have we done towards this outcome?" because there's no system that connects business context (aims, signals, guardrails, metis), code structure, and git history into a single queryable interface. The three exist in isolation — `.oh/` files, source code, and git log — and the agent has no way to intersect them.

**The shift:** From "prove the value of structured context" to "build the system that makes business context, code, and history queryable as one thing." The prototype isn't done until an agent can ask: *"What have we done to make alignment available via MCP?"* — and get back a grounded answer spanning aims, commits, and code.

### Markdown is the Lingua Franca

Markdown is both **input and output** for agents — this is a first-class architectural concern, not a formatting choice.

**As input (operational content — all markdown, not just `.oh/`):**
- CLAUDE.md, AGENTS.md, `.oh/` session files — **agent behavior configuration**
- README, ADRs, runbooks, changelogs — **project intent and conventions**
- These are not "docs" — they are **semantic context** that agents need to understand constraints, patterns, and decisions
- All markdown needs first-class parsing (pulldown-cmark) and embedding, not just `.oh/` files

**As output (query results):**
- MCP tool responses should be **markdown-native** — tables, `file:line` links, diff blocks
- The store is structured objects internally; markdown is the *view layer* for agent consumption

**Cross-references between markdown and code:**
- When CLAUDE.md says "always use `params![]` positional," that code span should link to actual code symbols in the index
- pulldown-cmark extracts code spans from markdown; the index matches them against the code symbol table
- This intersection is a differentiator — no existing tool (Codemogger, Srclight, code-memory) does this

### The Hybrid Index Model

Resolves the tension between Anthropic's "just-in-time context" and the VisualAge "persistent object model":

```
Lightweight persistent index  +  On-demand deep reads
= "know WHERE everything is"  +  "read WHAT you need when you need it"
```

- **Always indexed:** symbol names, file locations, dependency edges, heading hierarchy, change timestamps — small, fast, always current
- **On-demand:** full AST, embeddings, file contents, detailed type info — loaded through MCP tools when the agent needs them
- **This IS just-in-time context** — the index is the mechanism that makes just-in-time *possible*. Without it, "just-in-time" means "grep and hope."

### The Extractor Architecture

Pluggable extractors produce `{ metadata, chunks }` from files. Same pipeline handles code and markdown:

- **MarkdownExtractor** (pulldown-cmark): heading-delimited sections become chunks, code spans become cross-references, YAML frontmatter becomes structured metadata
- **TreeSitterExtractor**: functions/structs/traits/imports become symbols with metadata (file, line range, kind, parent scope), function bodies become embeddable chunks
- **Future extractors**: Terraform, YAML config, Dockerfile — anything with parseable structure
- The registry matches extractors to files by path/extension. Only changed files get re-extracted (git diff drives the delta).

### Constraints
- **Hard:**
  - **Repo-native** — `.oh/` in the repo, git-versioned, no external store
  - **Lightweight** — markdown files with frontmatter, complexity in tooling not content
  - **MCP interface** — agents query via MCP tools, not direct file reads
  - **Must connect all three layers** — aims + code + git history, and their intersections
  - **All markdown is first-class** — CLAUDE.md, README, session files parsed and indexed alongside `.oh/` and code

- **Soft:**
  - Rust + rust-mcp-sdk (matched to UHC patterns, but implementation choice)
  - YAML frontmatter (one serialization among several)
  - LanceDB for storage (could start simpler, but this is the target)
  - pulldown-cmark for markdown, tree-sitter for code (best current choices, not permanent commitments)

### What this framing enables
1. Building the prototype end-to-end — MCP server that reads `.oh/`, parses code and markdown, reads git history
2. The intersection query as the acceptance test: "what work relates to outcome X?"
3. Cross-references between prose and code: CLAUDE.md rules linked to the symbols they reference
4. Incremental delivery — can ship MCP tools as each layer comes online (files first, then git, then code parsing)
5. Semantic search across all content types: "find everything about error handling" spans code, markdown, and `.oh/` files

### What this framing excludes
- OH graph sync (Phase 5) — organizational memory is out of scope for the prototype
- DuckDB analytics overlay — optional, not required for the proof
- Real-time incremental indexing — full rebuild is fine for prototype scale
- LSP enrichment (type info, hover) — tree-sitter is sufficient for symbol extraction; LSP can layer on later

---

## Convergence with CodeState

CodeState and this aim are the same system viewed from two angles:

| CodeState asks | This aim asks |
|---|---|
| How do we make *code structure* queryable so agents stop making dumb mistakes? | How do we make *business intent* queryable so agents stay aligned to outcomes? |

The answer is the same architecture:
- **Structured artifacts** in the repo (code symbols <-> outcome/signal/constraint definitions)
- **Queryable via agent tooling** (MCP tools)
- **Git-aware** (symbol history <-> outcome evolution over commits)
- **Embeddings for semantic search** ("find code related to auth" <-> "find work related to reducing churn")
- **Markdown as the lingua franca** (code docs <-> business context docs)

The extractors don't just parse `.rs` and `.md` for code structure. They also parse `.oh/` files for business context. Same pipeline, different content.

**The harness IS the CodeState vision, applied to the full agent working context — not just code.**

### Evolved Architecture (from CodeState exploration)

```
git2              <- what changed? (replaces filesystem scanning for repos)
tree-sitter       <- parse code -> symbols
pulldown-cmark    <- parse markdown -> sections (including .oh/ business context)
LanceDB           <- store + search everything (columnar + vectors + full-text)
DuckDB (optional) <- SQL analytics overlay via lance-duckdb extension
MCP server        <- agent interface
```

---

## Repo Structure

```
my-project/
+-- .oh/                          <- the harness context
|   +-- outcomes/
|   |   +-- reduce-churn.md       <- aim + mechanism + signals
|   |   +-- api-reliability.md
|   +-- signals/
|   |   +-- p95-latency.md        <- SLO definition + thresholds
|   |   +-- onboarding-time.md
|   +-- guardrails/
|   |   +-- no-breaking-api.md    <- hard constraint
|   |   +-- require-migration.md
|   +-- metis/
|   |   +-- 2026-03-caching.md    <- "we tried X, learned Y"
|   |   +-- 2026-02-auth-flow.md
|   +-- sessions/
|       +-- current.md            <- active work context
+-- CLAUDE.md                     <- code conventions (existing)
+-- src/                          <- the code
+-- .git/                         <- history of everything above
```

---

## Connection to Artium/Ross Conversations

| Ross's concept | In this harness |
|---|---|
| "Reliability is the business goal" | Outcomes + signals make reliability measurable per project |
| "Evals and guardrails" | Guardrails as repo artifacts with severity levels |
| "Telemetric pricing" | Signals/SLOs provide the transaction-level value measurement |
| "The seed that grows per client" | `.oh/` directory IS the seed — starts minimal, accumulates metis |
| "Earned trust dial" | Guardrail severity starts at hard, relaxes as metis accumulates evidence |
| "Artisan convergence" | Shared outcomes/signals in repo give opinionated engineers shared context |

---

## Minimum Viable Harness

The smallest thing that proves the hypothesis:
1. One outcome file with structured frontmatter
2. One signal file with SLO definition
3. One MCP tool that returns them when queried
4. An agent that demonstrably scopes its work by reading them
5. After a session, one metis file written by the agent, curated by human

## Open Horizons Integration

This aim lives in the OH System context as an Aim under the OH System mission.

The repo-native `.oh/` structure is the *local projection* of the OH graph into a specific codebase. The sync model:
- `.oh/outcomes/` <-> OH Aims for this project
- `.oh/signals/` <-> OH Signals (once that initiative ships)
- `.oh/guardrails/` <-> OH Guardrails for this endeavor
- `.oh/metis/` <-> OH Metis entries for this endeavor

The repo is the working copy. OH is the organizational memory. Git push/pull semantics for sync (not real-time).

---

## Problem Space: MCP Bootstrap
**Updated:** 2026-03-06

### Objective
Time-to-self-reference: the MCP server becomes available to the coding agent ASAP, so the agent building it can use it while building it.

### Key Decisions
- **rust-mcp-sdk v0.8** — same as UHC, proven pattern with `#[mcp_tool]` macros + `ServerHandler` trait
- **HTTP transport** (like UHC) vs **stdio** — UHC uses HTTP via Axum; stdio is simpler for standalone CLI. Decision: start with HTTP to reuse UHC pattern verbatim.
- **No LanceDB/git2 for bootstrap** — just read `.oh/` files from disk. Embeddings and change detection come later.
- **YAML frontmatter + markdown body** — parsed with `serde_yaml` + `pulldown-cmark`

### Bootstrap Tool Surface
| Tool | Description | Read-only? |
|------|-------------|------------|
| `oh_get_outcomes` | List all outcomes with frontmatter + body | Yes |
| `oh_get_signals` | List all signals with SLO definitions | Yes |
| `oh_get_guardrails` | List all guardrails with severity | Yes |
| `oh_get_metis` | List all metis entries | Yes |
| `oh_get_context` | Return everything as one bundle | Yes |
| `oh_record_metis` | Write a new metis entry to `.oh/metis/` | No |

### Reference Implementation
UHC MCP server: `open-horizon-labs/unified-hifi-control/src/mcp/mod.rs` — exact same SDK, same macros, same patterns.

### Constraints
- Must use UHC's `rust-mcp-sdk` pattern (proven, same org)
- `.oh/` files are source of truth (repo-native guardrail)
- Must work with Claude Code's `.mcp.json` config
- No external dependencies for core function (no DB, no cloud)

---

## Full System Phases

### Phase 0: MCP Bootstrap (just read files)
- Read `.oh/` artifacts from disk, parse frontmatter, return via MCP tools
- 6 tools: get_outcomes, get_signals, get_guardrails, get_metis, get_context, record_metis
- No indexing, no embeddings — just structured file reads
- **Goal:** agent can query its own outcomes/constraints while building the rest

### Phase 1: Markdown Scanner + Embeddings
- pulldown-cmark parses all `.md` files (not just `.oh/`) into heading-delimited sections
- Each section becomes a chunk with metadata (file, heading hierarchy, byte range)
- LanceDB stores chunks with embeddings for semantic search
- **Goal:** "find markdown about error handling" or "what have we done towards alignment?"
- Cross-reference extraction: code spans in markdown link to code symbols

### Phase 2: Code Scanner (tree-sitter)
- tree-sitter parses code files (.rs, .ts, .py, etc.) into symbols (functions, structs, traits, imports)
- Symbols stored in LanceDB with metadata (file, line range, kind, parent scope)
- Embeddings on function/struct bodies for semantic code search
- **Goal:** "find functions related to authentication" or "what calls Database::get_connection?"

### Phase 3: Git Awareness
- git2 integration for change detection (replaces filesystem scanning)
- `git diff` drives incremental re-indexing (only re-parse/re-embed changed files)
- Commit history mapped to symbols: "when did this function last change? who changed it?"
- Blame integration: per-symbol authorship
- **Goal:** "what changed in the last 3 commits?" or "who last touched the auth module?"

### Phase 4: Query Engine
- Hybrid queries: structured filters + vector similarity in one call
- DuckDB as optional analytics overlay via lance-duckdb extension
- Natural language queries: "what have we done to make alignment available via MCP?"
  - Decomposes to: search metis + git history + code changes matching "alignment" + "MCP"
  - Returns: timeline of relevant commits, metis entries, code changes, outcome progress
- **Goal:** agents can ask open-ended questions about project state and get grounded answers

### Phase 5: OH Sync
- Bidirectional sync between `.oh/` repo artifacts and OH graph
- `.oh/outcomes/` <-> OH Aims, `.oh/signals/` <-> OH Signals, etc.
- Git push/pull semantics (not real-time)
- **Goal:** organizational memory and repo-local context stay in sync

---

## The Query That Proves It Works

> "What have we done to make alignment available via MCP?"

This query exercises the full stack:
1. **Outcome lookup** — finds the "agent-alignment" outcome in `.oh/outcomes/`
2. **Metis search** — finds learnings tagged to that outcome
3. **Git history** — finds commits whose messages or changed files relate to "alignment" + "MCP"
4. **Code search** — finds MCP tool definitions, handler implementations
5. **Markdown search** — finds session file sections discussing MCP bootstrap
6. **Synthesis** — assembles a timeline of progress towards the outcome

When this query returns a useful answer, the system works.
