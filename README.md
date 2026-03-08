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

**The graph:** `search_symbols` and `graph_query` expose a multi-language code graph — symbols, imports, topology boundaries — built by incremental scanning with tree-sitter across 22 languages. LSP enrichment adds cross-file call edges and synthesizes virtual nodes for external package symbols (`tokio::Runtime`, `lancedb::Connection`), so `graph_query` traversal crosses package boundaries.

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

Runs 11 checks end-to-end: scanner init, file walk, symbol extraction, graph index, embedding index, each MCP tool category, and a worktree smoke test. Exits 0 on pass, 1 on failure. Safe to run in CI.

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

**Extraction (pluggable, 22 extractors):**
- **tree-sitter (code)** — Rust, Python, TypeScript/TSX, JavaScript/JSX, Go, Java, Bash, Ruby, C++, C#, Kotlin, Zig, Lua, Swift (functions, classes, imports, name column for precise LSP cursors)
- **tree-sitter (config/infra)** — HCL/Terraform, JSON, TOML, YAML (with Kubernetes manifest detection)
- **Markdown** — heading-aware sections with YAML frontmatter
- **Schema** — .proto messages, SQL tables, OpenAPI endpoints
- **Topology** — `Command::new`, `TcpListener`, `tokio::spawn` → architecture edges
- **LSP** — cross-file call edges + virtual external package nodes (see below)
- **Embeddings** — fastembed-rs (BAAI/bge-small-en-v1.5, local ONNX, no API key)

**Graph (LanceDB + petgraph):**
- Nodes: symbols, schemas, artifacts, PR merges
- Edges: calls, implements, depends-on, modified, serves (with provenance + confidence)
- In-memory traversal via petgraph (microseconds)

**LSP enrichment (37 servers auto-detected from PATH):**

RNA auto-discovers installed language servers and enriches the graph with cross-file edges. No configuration — if the binary is on PATH, it's used. Missing servers skipped gracefully.

What LSP adds beyond tree-sitter:
- `callHierarchy/incomingCalls` → who calls this function (`Calls` edges)
- `callHierarchy/outgoingCalls` → what does this function call (`Calls` edges) + synthesizes **virtual nodes** for external package symbols (`tokio::Runtime`, `lancedb::Connection`) so `graph_query` traversal crosses package boundaries
- `textDocument/implementation` → who implements this trait (`Implements` edges)
- `textDocument/documentLink` → cross-document references for markdown/docs (`DependsOn` edges)

Virtual external nodes have stable IDs (`external::{package}::{fqn}`) and are upserted on each scan — no manual configuration needed. They appear in graph traversal but not semantic search (no body to embed).

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

**Scanner (incremental, worktree-aware):**
- mtime-based subtree skipping — unchanged directories skipped entirely
- git diff as precision layer when `.git` present
- **Worktree awareness** — active git worktrees auto-detected from `.git/worktrees/` and indexed as separate roots; agents running in worktrees see their own in-progress changes via `search_symbols`
- Configurable excludes via `.oh/config.toml`
- Rescans in <1s after initial scan

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

- 9 intent-based MCP tools, 155 tests
- 22 extractors: Rust, Python, TypeScript, JavaScript, Go, Java, Bash, Ruby, C++, C#, Kotlin, Zig, Lua, Swift, HCL/Terraform, JSON, TOML, YAML, Markdown, Proto, SQL, OpenAPI
- LSP enrichment: cross-file `Calls` edges + virtual external package nodes for cross-boundary graph traversal
- `graph_query(mode: "impact")` finds callers across codebase and into external packages
- `rna test --repo .` — 11-check pipeline verifier (scanner → extract → embed → each tool category → worktree smoke test)
- Worktree awareness — agents in git worktrees see their own in-progress symbols via `search_symbols`
- Graph + embeddings persisted to LanceDB — loads in <1s on restart, no re-embedding
- LSP metadata (resolved types, hover docs) included in embeddings — semantic search finds type-level concepts
- Incremental updates within a session — edit a file, next tool call reflects it
- Background scanner (15min) keeps index warm during long sessions
- Multi-root workspace scanning via `~/.config/rna/roots.toml` + auto-detected git worktrees
- Semantic search via local embeddings (no API key)
- Context auto-injected on first MCP tool call — agents always see business context
- Validated on 3 repos with different shapes (Rust, Python+TS monorepo)

## Design Notes

- [`docs/rna-source-compatibility.md`](docs/rna-source-compatibility.md) — source-capability design for future Context Assembler integration
