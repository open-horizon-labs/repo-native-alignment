# Repo-Native Alignment

Aim-conditioned decision infrastructure for coding agents. Agents don't just execute — they plan and adapt conditioned on declared aims, treating repo artifacts as evidence that updates confidence in whether the current aim framing is still correct.

We don't build features, we build capabilities.

## How It Works

```
┌─────────────────────────────────────────────────────────────┐
│  OH MCP (organizational)        RNA MCP (repo-local)        │
│  ─ aims, missions, endeavors    ─ outcomes, signals, code   │
│  ─ cross-project context        ─ workspace graph (petgraph)│
│  ─ decision logs                ─ multi-lang AST + topology │
│                                 ─ semantic search over .oh/ │
└──────────┬──────────────────────────────┬───────────────────┘
           │                              │
           ▼                              ▼
┌─────────────────────────────────────────────────────────────┐
│  OH Skills (cross-cutting)      OH Agents (phase-isolated)  │
│  /review  /dissent  /salvage    oh-aim  oh-execute  oh-ship │
│  ─ run in main session          ─ own context + scoped tools│
│  ─ need conversation context    ─ read/write .oh/ sessions  │
│  ─ use RNA+OH MCP when avail    ─ use RNA+OH MCP when avail │
└─────────────────────────────────────────────────────────────┘
           │                              │
           ▼                              ▼
┌─────────────────────────────────────────────────────────────┐
│  .oh/ directory (repo-local cache, git-versioned)           │
│  outcomes/ ─ what we're optimizing for                      │
│  signals/  ─ how we measure progress                        │
│  guardrails/ ─ constraints that shape behavior              │
│  metis/    ─ learnings that compound across sessions        │
│  config.toml ─ scanner excludes, per-project tuning         │
└─────────────────────────────────────────────────────────────┘
```

**The loop:** Skills guide work → MCP tools read/write context → `.oh/` accumulates learnings → git versions everything → next session starts richer.

**The graph:** `search_symbols` and `graph_query` expose a multi-language code graph — symbols, calls, imports, topology boundaries — across 22 languages. Graph traversal follows call chains into external packages (`tokio`, `lancedb`, etc.), so impact analysis doesn't stop at your repo boundary.

**The join:** `outcome_progress` connects layers structurally — outcome → file patterns → tagged commits → code symbols → PR merges. Structural links, not keyword matching.

**Aim feedback:** Repo artifacts are evidence against declared aims. Commits, symbols, PR merges, and metis/guardrails don't just show activity — they update confidence in whether the current aim framing is still correct. When evidence diverges from the aim, that's a signal to reframe, not just push harder.

**The search:** `oh_search_context` finds relevant context by natural language — "guardrails about API compatibility" — instead of listing all artifacts and filtering manually.

## Quick Start

### Prerequisites

| Dependency | Install |
|------------|---------|
| `cargo` | [rustup.rs](https://rustup.rs) |
| `protoc` | `brew install protobuf` / `apt install protobuf-compiler` |
| `npx` | ships with Node.js — [nodejs.org](https://nodejs.org) |

### 1. Install

```bash
git clone https://github.com/open-horizon-labs/repo-native-alignment.git
cd repo-native-alignment
cargo install --locked --path .
```

### 2. Set up a project

```bash
repo-native-alignment setup --project /path/to/your/project
```

This checks dependencies, installs the binary, configures `.mcp.json`, and verifies everything works. Safe to re-run on updates.

Preview first: `repo-native-alignment setup --project . --dry-run`

### 3. Teach your agents

Open a Claude Code session in your project:

```
/teach-oh
```

This explores your codebase, asks about your aims, writes `AGENTS.md`, scaffolds `.oh/` with outcomes and constraints, and installs phase agents.

### 4. Verify the pipeline

```bash
repo-native-alignment test --repo /path/to/your/project
```

Runs 22+ checks end-to-end: scanner init, file walk, symbol extraction, graph index, embedding index, each MCP tool category, worktree smoke, incremental persist, virtual external node round-trip, HEAD-change detection, cross-language constants, semantic search, and more. Exits 0 on pass, 1 on failure. Safe to run in CI.

### 5. Start working

The system compounds from here. Agents use `oh_search_context` to discover relevant context, `search_symbols` to explore code, and `oh_record` to capture learnings. Each session starts richer than the last.

---

**Manual path:** Build from source (`cargo build --release`), add `rna-server` to your `.mcp.json` pointing at the binary with `--repo <project path>`. The setup command automates this.

## RNA MCP Server — 9 tools

The repo-local intelligence layer. Scans your repo, extracts a multi-language code graph, and serves everything via MCP.

| Category | Tools |
|----------|-------|
| **Context** | `oh_get_context` — all business artifacts in one call |
| **Search** | `oh_search_context` — semantic search over .oh/ + commits (optionally code + markdown) |
| **Write** | `oh_record` — record metis, signals, guardrails, or update outcomes |
| **Scaffold** | `oh_init` — initialize .oh/ directory from project context |
| **Code** | `search_symbols` — multi-language symbol search with graph edges |
| **Graph** | `graph_query` — traverse neighbors, impact analysis, reachability |
| **History** | `oh_search_context` returns commit hash — use `git show <hash>` via Bash for diffs |
| **Join** | `outcome_progress` — structural join: outcome → commits → symbols → PRs |
| **Workspace** | `list_roots` — show configured workspace roots |

**Extraction (pluggable):**
- **Code** — Rust, Python, TypeScript/TSX, JavaScript/JSX, Go, Java, Bash, Ruby, C++, C#, Kotlin, Zig, Lua, Swift
- **Config & infra** — HCL/Terraform, JSON, TOML, YAML (Kubernetes manifests detected automatically)
- **Docs & schema** — Markdown (heading-aware), .proto, SQL, OpenAPI
- **Architecture** — subprocess, network, async boundaries detected as topology edges
- **LSP** — cross-file call chains; graph traversal follows calls into external packages
- **Embeddings** — local ONNX, no API key needed

**Constants and literals (cross-language):**

All 22 extractors index constants and literal values. `search_symbols` returns the value inline:

```
- const MAX_RETRIES (rust) src/config.rs:12  Value: `5`
- const MAX_RETRIES (python) settings.py:3   Value: `5`
- const MAX_RETRIES (go) config.go:8         Value: `5`
```

Named constants are declared identifiers — `const MAX_RETRIES = 5`, static final fields, ALL_CAPS module-level assignments, etc.

Synthetic constants are inferred from structure — YAML/TOML/JSON top-level scalar values, OpenAPI enum values, and single-token string literals (e.g. `"application/json"`, `"GET"`) found in function bodies. They appear with a `*(literal)*` badge.

`search_symbols` accepts a `synthetic` filter to narrow results to declared constants, inferred literals, or both.

Language mapping:
- **Rust** — `const_item` with extracted value
- **Python** — module-level ALL_CAPS assignments (`[A-Z][A-Z0-9_]+`)
- **TypeScript/JavaScript** — module-level `const` declarations
- **Go** — `const_spec` inside `const_declaration`
- **Java** — `static final` field declarations
- **Kotlin** — `const val` property declarations
- **C#** — `const` field declarations
- **Swift** — module-level `let` bindings
- **Zig** — `const` variable declarations
- **C/C++** — `constexpr` and `static const` declarations
- **Lua/Ruby/Bash** — ALL_CAPS module-level assignments
- **HCL** — `variable` block default values
- **Proto** — enum values and `option` fields
- **SQL** — `CREATE TYPE ... AS ENUM` values
- **YAML/TOML/JSON/OpenAPI** — top-level scalar values (synthetic)

**Graph (LanceDB + petgraph):**
- Nodes: symbols, schemas, artifacts, PR merges
- Edges: calls, implements, depends-on, modified, serves (with provenance + confidence)
- In-memory traversal via petgraph (microseconds)

**LSP enrichment (37 servers auto-detected from PATH):**

RNA auto-discovers installed language servers and enriches the graph with cross-file edges. No configuration — if the binary is on PATH, it's used. Missing servers skipped gracefully.

What LSP adds beyond tree-sitter:
- **Who calls this?** — inbound call graph, not just the function definition
- **What does this call?** — outbound call chain, including into external packages (`tokio`, `lancedb`, your dependencies)
- **Who implements this trait/interface?** — implementation edges across files
- **Doc cross-references** — links between markdown documents and code

The result: `graph_query(mode: "impact", from: "my_fn")` shows the blast radius of a change, following call chains across your entire codebase and into the packages it depends on.

Common servers (install for richer graphs):

| Language | Server | Install |
|---|---|---|
| Rust | rust-analyzer | `rustup component add rust-analyzer` |
| Python | pyright | `npm install -g pyright` |
| TypeScript/JS | typescript-language-server | `npm install -g typescript-language-server typescript` |
| Go | gopls | `go install golang.org/x/tools/gopls@latest` |
| C/C++ | clangd | ships with LLVM / `brew install llvm` |
| Markdown | marksman | `brew install marksman` |

Plus 31 more: Ruby (solargraph), Java (jdtls), Kotlin, Lua, Zig, Elixir, Haskell, OCaml, Scala, Dart, PHP, Swift, Nix, Terraform, TOML, YAML, and others. Full list in `src/extract/mod.rs`.

**Scanner (incremental, event-driven, worktree-aware):**
- Rescans in <1s — only changed files re-extracted and upserted (O(changed files) end-to-end, including LanceDB)
- Event-driven reindex — triggers immediately on `git pull`, `git merge`, or branch checkout; 15-minute heartbeat is the fallback, not the trigger
- Git worktrees indexed automatically — agents running parallel branches see their own in-progress symbols, not the stale main-branch index
- Self-healing cache — schema changes trigger automatic rebuild; no manual cache deletion needed
- Configurable excludes via `.oh/config.toml`

## Companion Systems

### [OH MCP](https://github.com/cloud-atlas-ai/oh-mcp-server) — organizational context

Missions, aims, endeavors, decision logs, cross-project context. RNA's `.oh/` is the repo-local projection of the OH graph.

### [OH Skills](https://github.com/open-horizon-labs/skills) — workflow skills

| Skill | What it does |
|-------|-------------|
| `/aim` | Frame the outcome you want |
| `/review` | Check alignment before committing |
| `/dissent` | Seek contrary evidence before one-way doors |
| `/salvage` | Extract learning before restarting |
| `/solution-space` | Evaluate approaches before committing |
| `/execute` | Build with pre-flight checks and drift detection |

### OH Phase Agents — isolated execution

Agent wrappers for each workflow phase (`oh-aim`, `oh-execute`, `oh-ship`, etc.). Each runs in its own context window with scoped tools. Installed to `.claude/agents/`.

## The `.oh/` Directory

```
.oh/
├── outcomes/        <- what we're optimizing for (YAML frontmatter + markdown)
├── signals/         <- how we measure progress (SLO definitions + observations)
├── guardrails/      <- constraints that shape behavior (hard/soft/candidate)
├── metis/           <- learnings that compound (the institutional memory)
├── config.toml      <- scanner excludes, per-project tuning
└── .cache/          <- scan state, embedding index (gitignored)
```

Outcomes declare `files:` patterns linking to code. Commits tag `[outcome:X]` linking to outcomes. These structural links power `outcome_progress`.

**Aims-aware context assembly:** On first tool call, agents receive aims + relevant artifact evidence + recent metis/guardrails, so planning is conditioned on current strategic intent, not just local code state.

`.oh/` is a **cache**, not source of truth. `rm -rf .oh/` loses context but breaks nothing.

### Scanner Configuration

```toml
# .oh/config.toml
[scanner]
exclude = [".omp/", "data/", "*.log"]   # added to defaults
include = ["vendor/"]                     # opt back into something excluded by default
```

Default excludes: `node_modules/`, `.venv/`, `target/`, `build/`, `__pycache__/`, `.git/`, `.claude/`, `.omp/`, `dist/`, `vendor/`, `.build/`, `.cache/`

## Architecture

```
Scanner (mtime + git)     <- incremental file change detection
  ├── tree-sitter         <- Rust, Python, TS, Go → symbols + import graph
  ├── schema extractors   <- .proto, SQL, OpenAPI → schema nodes + edges
  ├── topology detector   <- subprocess/network/async boundaries → architecture edges
  ├── markdown extractor  <- heading sections + YAML frontmatter → graph nodes
  ├── LSP enricher        <- 37 servers auto-detected → calls, implements, doc links
  └── fastembed-rs        <- local ONNX embeddings (BAAI/bge-small-en-v1.5)
         │
         ▼
Graph (LanceDB + petgraph)
  ├── LanceDB             <- columnar + vector store
  ├── petgraph            <- in-memory traversal (BFS, impact, reachability)
  └── SourceEnvelope      <- canonical records with scope + provenance
         │
         ▼
MCP Server (rust-mcp-sdk) <- stdio + HTTP transport, 9 tools
```

No cloud dependency. Everything local, git-versioned, disposable.

## Status

**Working today:**
- 9 MCP tools, 22 language extractors, 177 tests
- `outcome_progress` joins outcomes → commits → symbols → PRs structurally
- `graph_query(mode: "impact")` traces blast radius across your codebase and into external packages
- `search_symbols` returns results from active git worktrees — parallel agents see their own changes
- Cross-language constant + literal search — find `MAX_RETRIES = 5` in Rust, Python, Go, YAML in one query; filter by declared vs inferred
- Semantic search over code, docs, and business artifacts — no API key, runs locally
- `rna test --repo .` verifies the full pipeline (22+ checks) in one command
- Index persists between sessions — restarts in <1s, auto-rebuilds on schema change
- Event-driven reindex — responds to HEAD changes immediately, not on a timer
- Context injected on first tool call — agents start every session with business context
- Validated on 3 repos: Rust CLI, Python+TS monorepo, self-referential (this repo)

## Design Notes

- [`docs/rna-source-compatibility.md`](docs/rna-source-compatibility.md) — source-capability design for future Context Assembler integration
