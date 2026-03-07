# Repo-Native Alignment

Agents stay aligned to declared business outcomes — not just code correctness — because outcomes, constraints, signals, and learnings live *in the repo* as queryable, evolving artifacts.

We don't build features, we build capabilities.

## How It Works

Four systems collaborate. Each is independent; together they compound.

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

**The loop:** Skills/agents guide the workflow → MCP tools read/write structured context → `.oh/` accumulates learnings → git versions everything → next session starts richer.

**The join:** `outcome_progress` connects layers structurally — outcome → file patterns → tagged commits → code symbols → related markdown. Not keyword matching; structural links.

**The graph:** `search_symbols` and `graph_query` expose a multi-language code graph — symbols, imports, topology boundaries — built by incremental scanning with tree-sitter extraction across Rust, Python, TypeScript, and Go.

**The search:** `oh_search_context` lets agents describe what they need in natural language — "guardrails about API compatibility" — instead of listing all artifacts and filtering manually.

## Quick Start

### Prerequisites

`repo-native-alignment setup` checks for these before doing anything:

| Dependency | Install |
|------------|---------|
| `cargo` | [rustup.rs](https://rustup.rs) |
| `protoc` | `brew install protobuf` / `apt install protobuf-compiler` |
| `npx` | ships with Node.js — [nodejs.org](https://nodejs.org) |

### 1. Install the RNA binary (one-time)

```bash
git clone https://github.com/open-horizon-labs/repo-native-alignment.git
cd repo-native-alignment
cargo install --locked --path .
```

### 2. Bootstrap a project

Run once per project (idempotent — safe to re-run on updates):

```bash
repo-native-alignment setup --project /path/to/your/project
```

This does, in order:

1. Preflights required tools for the selected actions: checks `cargo` + `protoc` when RNA source is available for reinstall, and checks `npx` unless `--skip-skills` is set.
2. Installs/updates the RNA binary via `cargo install --locked --path <rna repo root>` when source is available; otherwise reuses an already-installed binary.
3. Installs OH skills globally: `npx skills add open-horizon-labs/skills -g -a claude-code -y`.
4. Writes or merges `<project>/.mcp.json`, adding the `rna-server` entry and preserving any existing servers.
5. Verifies the configured binary responds to `--help` and that `.mcp.json` contains `rna-server`.

**Preview without making changes:**

```bash
repo-native-alignment setup --project . --dry-run
```

**Skip OH skills install** (if already installed or managing separately):

```bash
repo-native-alignment setup --project . --skip-skills
```

### 3. Run `/teach-oh`

Open a Claude Code session in your project and run:

```
/teach-oh
```

This does everything:
- Explores your codebase (stack, patterns, conventions)
- Asks about your aims and constraints
- Writes `AGENTS.md` with project context
- Scaffolds `.oh/` with outcomes, signals, guardrails
- Installs phase agents to `.claude/agents/`

### 4. Start working

The system compounds from here. Skills and agents use `oh_search_context` to discover relevant context. `search_symbols` and `graph_query` expose the code graph. `oh_record` records what you learn. Next session starts richer.

---

**Manual path (fallback):** If you prefer full control, build from source (`cargo build --release`), then hand-edit your project's `.mcp.json` to add `rna-server` pointing at the compiled binary with `--repo <project path>`. The setup command exists to make this deterministic and verifiable, not to replace understanding of what it configures.

## Design Notes

- [`docs/rna-source-compatibility.md`](docs/rna-source-compatibility.md) — steers the workspace-engine roadmap so RNA stays a strong local MCP runtime now and a clean future Context Assembler source later

## The Four Systems

### RNA MCP Server (this repo) — 20 tools + workspace graph

The repo-local intelligence layer. Incrementally scans your repo, extracts a multi-language code graph, and serves it via MCP.

**Extraction stack (pluggable, multi-language):**
- **tree-sitter** — Rust, Python, TypeScript/TSX, Go symbol extraction (functions, structs, traits, classes, interfaces, imports)
- **Markdown** — heading-aware sections with YAML frontmatter as graph nodes
- **Topology detection** — `Command::new`, `TcpListener::bind`, `tokio::spawn` patterns → runtime architecture edges
- **Embeddings** — fastembed-rs (BAAI/bge-small-en-v1.5) for semantic search over `.oh/` artifacts

**Graph model (LanceDB + petgraph):**
- **Nodes:** symbols, components, schemas, artifacts, PR merges
- **Edges:** calls, implements, depends-on, connects-to, modified, serves (with provenance + confidence)
- **Traversal:** in-memory petgraph for BFS, impact analysis, reachability (microseconds)
- **Source-capable:** `SourceEnvelope` with scope, idempotency keys, provenance for future Context Assembler integration

**Scanner (incremental, mtime + git):**
- mtime-based subtree skipping — unchanged directories skipped entirely
- git diff as precision layer when `.git` present
- Configurable excludes via `.oh/config.toml` (default: `node_modules/`, `.venv/`, `target/`, `.git/`, `.claude/`, `.omp/`)
- State persisted to `.oh/.cache/scan-state.json` — subsequent scans in <1s

| Category | Tools |
|----------|-------|
| **Read .oh/** | `oh_get_context` (all artifacts in one call) |
| **Write .oh/** | `oh_record` (type: metis/signal/guardrail/outcome), `oh_init` |
| **Search** | `oh_search_context` (semantic, optionally include code + markdown), `git_history` (commits + file history) |
| **Semantic Search** | `oh_search_context` — natural language search over `.oh/` artifacts + git commits |
| **Graph** | `search_symbols` (multi-lang), `graph_query` (neighbors/impact/reachable) |
| **Join** | `outcome_progress` — the structural intersection query |

### [OH MCP](https://github.com/cloud-atlas-ai/oh-mcp-server) — organizational context

The organizational memory layer. Missions, aims, endeavors, decision logs, cross-project context. RNA's `.oh/` is the repo-local projection of the OH graph.

| What OH provides | How RNA uses it |
|-----------------|-----------------|
| Aims and endeavors | `oh_init` seeds `.oh/outcomes/` from the OH graph |
| Decision logs | Agents log decisions via `oh_log_decision` |
| Cross-project context | Agents see which other projects share this aim |

### [OH Skills](https://github.com/open-horizon-labs/skills) — cross-cutting workflow

Prompt-based skills that run in the main conversation. They need full conversation context to detect drift, challenge decisions, and extract learning.

| Skill | What it does | RNA MCP integration |
|-------|-------------|-------------------|
| `/aim` | Frame the outcome | Calls `oh_search_context("outcomes related to [task]")` to find relevant outcomes |
| `/review` | Check alignment before committing | Calls `oh_search_context("guardrails for [area]", types: ["guardrail"])`, `outcome_progress` |
| `/dissent` | Devil's advocate before one-way doors | Calls `oh_search_context("constraints and risks for [decision]")` to ground dissent |
| `/salvage` | Extract learning before restarting | Records metis and guardrail candidates via write tools |
| `/solution-space` | Evaluate approaches | Calls `oh_search_context("past approaches to [problem]", types: ["metis"])` for prior art |
| `/execute` | Build the change | Pre-flight via `oh_search_context("guardrails for [scope]", types: ["guardrail"])`, tags commits |

### OH Phase Agents — isolated execution

Agent wrappers that run each workflow phase in its own context window with scoped tools. Installed to `.claude/agents/` for Claude Code.

| Agent | Phase | RNA MCP integration |
|-------|-------|-------------------|
| `oh-aim` | Frame the outcome | Reads outcomes first, updates after |
| `oh-problem-space` | Map constraints | Loads full context via `oh_get_context` |
| `oh-problem-statement` | Define the framing | Reads outcomes + guardrails |
| `oh-solution-space` | Evaluate approaches | Validates against guardrails, records decision as metis |
| `oh-execute` | Build | Pre-flight guardrail check, tags commits `[outcome:X]` |
| `oh-ship` | Deliver | Records signal observations, updates outcome status |

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

`.oh/` is a **cache**, not source of truth. Outcomes originate in external systems (OH graph, Jira, Linear). `.oh/` is the repo-local, git-versioned projection. `rm -rf .oh/` loses context but breaks nothing.

### Scanner Configuration

Add `.oh/config.toml` to customize what gets scanned:

```toml
[scanner]
exclude = [".omp/", "data/", "*.log"]   # added to defaults
include = ["vendor/"]                     # opt back into something excluded by default
```

Default excludes: `node_modules/`, `.venv/`, `target/`, `build/`, `__pycache__/`, `.git/`, `.claude/`, `.omp/`, `dist/`, `vendor/`, `.build/`, `.cache/`, `*.pyc`, `*.o`, `*.so`, `*.dylib`, `.DS_Store`

## Architecture

```
Scanner (mtime + git)     <- incremental file change detection
  ├── tree-sitter         <- Rust, Python, TS, Go → symbols + import graph
  ├── topology detector   <- Command::new, TcpListener, tokio::spawn → architecture edges
  ├── markdown extractor  <- heading sections + YAML frontmatter → graph nodes
  └── fastembed-rs        <- local ONNX embeddings (BAAI/bge-small-en-v1.5)
         │
         ▼
Graph (LanceDB + petgraph)
  ├── LanceDB             <- columnar + vector store (symbols, edges, pr_merges, file_index)
  ├── petgraph            <- in-memory traversal (BFS, impact, reachability)
  └── SourceEnvelope      <- canonical records with scope + provenance
         │
         ▼
MCP Server (rust-mcp-sdk) <- stdio + HTTP transport, 20 tools
```

No cloud dependency. Everything is local, git-versioned, and disposable. The embedding model downloads once on first use and runs on CPU.

## Status

**Validated on 3 repos** — repo-native-alignment (self-referential), fspulse (Rust CLI), innovation-connector (enterprise Python+TS monorepo). Cold-started on all three, returned real context.

- 20 MCP tools, 97 tests, stdio + HTTP transport
- Multi-language extraction: Rust, Python, TypeScript/TSX, Go, Markdown
- Incremental scanner with mtime skip + git optimization + configurable excludes
- Unified graph model: code symbols + topology + schemas + business context + PR merges
- In-memory petgraph traversal: neighbors, reachability, impact analysis
- Semantic search via fastembed-rs + LanceDB (local ONNX, no API key)
- Source-capable: SourceEnvelope with scope, provenance, idempotency keys (#17)
- Full read-write feedback loop exercised on real work
- 10 metis entries, 8 guardrails, 2 signals — all recorded via MCP tools

**Known gaps:**
- LSP enricher trait defined but not implemented (tree-sitter only — `Calls`/`Implements` edges need LSP)
- Schema extractors (.proto, SQL, OpenAPI) not yet implemented
- Multi-root workspace scanning designed (#12) but single-repo only today
- PR merge extraction designed (graph types exist) but not wired to git history walking
- `setup` command configures RNA MCP only — OH MCP server install requires a separate step
