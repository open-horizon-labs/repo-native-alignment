# Repo-Native Alignment

[![CI](https://github.com/open-horizon-labs/repo-native-alignment/actions/workflows/rust-main-merge.yml/badge.svg)](https://github.com/open-horizon-labs/repo-native-alignment/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Aim-conditioned decision infrastructure for coding agents. A single binary MCP server that connects business outcomes to code — so agents know *why* they're building, not just *what*.

No Docker. No external database. No API key. `cargo install` and go.

**[Quick Start](#quick-start)** | **[What Agents Can Ask](#what-agents-can-ask)** | **[MCP Tools](#rna-mcp-server--5-tools)** | **[The .oh/ Directory](#the-oh-directory)** | **[Compared To](#compared-to)** | **[Docs](#detailed-documentation)**

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

## What Agents Can Ask

Once RNA is running, agents query your codebase and business context through MCP tool calls. Here's what that looks like in practice:

### Finding code

- "Where is the authentication handler?" → `search_symbols("AuthHandler")` → file, line, signature, graph edges
- "Find functions related to payment processing" → `oh_search_context("payment processing", include_code=true)` → ranked results scored 0-1, production code before tests, function bodies searched by meaning
- "How does scanning work?" → `oh_search_context("scanning", include_code=true, include_markdown=true)` → implementation code and doc sections, not test files
- "Show me all structs in embed.rs" → `search_symbols("", kind="struct", file="embed.rs")` → every struct with edges

### Understanding impact

- "What depends on the database connection pool?" → `graph_query(query="database connection pool", mode="impact")` → transitive dependents (no node_id lookup needed)
- "What calls AuthHandler?" → `graph_query(query="AuthHandler", direction="incoming")` → callers, implementors
- "Find all trait implementors" → `graph_query(query="Enricher trait", edge_types=["implements"])` → concrete types with compiler-grade `Implements` edges from LSP

### Connecting code to business outcomes

- "How is the agent-alignment outcome progressing?" → `outcome_progress("agent-alignment")` → tagged commits → changed files → symbols → PRs
- "Find signals related to reliability" → `oh_search_context("reliability", artifact_types=["signal"])` → measurement definitions
- "What are our constraints?" → `oh_search_context("constraints guardrails")` → all guardrails ranked by relevance

### Knowing when to trust results

Every query includes freshness metadata so agents know what they're working with:

```
Index: 5373 symbols · last scan 2m ago · LSP: enriched (3307 edges) · schema v3
```

During cold start, semantic tools return actionable status instead of errors:

```
Embedding index: building — semantic results will appear shortly. Retry in a few seconds.
```

Agents see `LSP: pending` and know to retry for complete call graphs, or `LSP: enriched (3307 edges)` and know results are compiler-accurate.

## Quick Start

### 1. Install

**Claude Code users** (recommended):

```bash
# 1. Add the marketplace
claude plugin marketplace add open-horizon-labs/repo-native-alignment

# 2. Install the plugin
claude plugin install rna-mcp

# 3. Restart Claude Code, then run the setup skill:
/rna-mcp:setup
```

Setup detects your platform (optimized binary for M2+ chips with bf16/i8mm), downloads the binary to `~/.cargo/bin/`, configures `.mcp.json`, and updates AGENTS.md with tool guidance.

**Download a prebuilt binary** (manual):

```bash
# macOS Apple Silicon (M2+ optimized — bf16/i8mm)
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-darwin-arm64-fast.tar.gz | tar xz -C ~/.cargo/bin

# macOS Apple Silicon (M1 baseline)
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-darwin-arm64.tar.gz | tar xz -C ~/.cargo/bin

# Linux x86_64
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-linux-x86_64.tar.gz | tar xz -C ~/.cargo/bin
```

**Build from source** (requires [Rust toolchain](https://rustup.rs)):

```bash
git clone https://github.com/open-horizon-labs/repo-native-alignment.git
cd repo-native-alignment
cargo install --locked --path .
```

### 1b. Connect to your MCP client

The MCP server command is `repo-native-alignment` with `--repo` as an argument. When your MCP client asks for a command, enter `repo-native-alignment` as the command and `--repo /path/to/your/project` as args.

**Important:** `command` must be just the binary name. The `--repo` flag goes in `args`, not in `command` — MCP stdio transport doesn't do shell splitting.

Example `.mcp.json`:

```json
{
  "mcpServers": {
    "rna": {
      "type": "stdio",
      "command": "repo-native-alignment",
      "args": ["--repo", "/path/to/your/project"]
    }
  }
}
```

For HTTP transport: `repo-native-alignment --repo . --transport http --port 8382`

### 1c. Try it from the CLI

Before wiring up MCP, evaluate RNA directly from the terminal:

```bash
repo-native-alignment search "auth" --repo /path/to/your/project
repo-native-alignment graph --node "<stable-id-from-search>" --mode impact --repo .
repo-native-alignment scan --path /path/to/your/project
```

### 2. Set up a project

```bash
repo-native-alignment setup --project /path/to/your/project
```

This checks dependencies, installs the binary, configures `.mcp.json`, and verifies everything works. Safe to re-run on updates.

Preview first: `repo-native-alignment setup --project . --dry-run`

### 3. Teach your agents (optional — requires [OH Skills](https://github.com/open-horizon-labs/skills))

Install OH Skills ([see instructions](https://github.com/open-horizon-labs/skills#install)), then open a Claude Code session in your project and run:

```
/teach-oh
```

This explores your codebase, asks about your aims, writes `AGENTS.md`, scaffolds `.oh/` with outcomes and constraints, and installs phase agents. RNA tools are automatically detected and used during exploration if installed.

### 4. Verify the pipeline

```bash
repo-native-alignment test --repo /path/to/your/project
```

Runs 25 checks end-to-end. Exits 0 on pass, 1 on failure. Safe to run in CI.

### 5. Start working

The system compounds from here. Agents use `oh_search_context` to discover relevant context, `search_symbols` to explore code, and write learnings to `.oh/metis/`. Each session starts richer than the last.

## RNA MCP Server — 5 tools

| Tool | What it's for |
|------|--------------|
| `oh_search_context` | Find relevant context by meaning: search .oh/ artifacts, commits, code, and markdown in one query |
| `search_symbols` | Find code symbols by name or signature, get file locations and graph edges |
| `graph_query` | Trace code relationships: what calls this, what depends on it, what's reachable |
| `outcome_progress` | Connect business outcomes to code: outcome → tagged commits → changed files → symbols |
| `list_roots` | Show which workspace roots are configured and their scan status |

### Plugin Skills

| Skill | What it does |
|-------|-------------|
| `/rna-mcp:setup` | Download binary, configure MCP, update AGENTS.md |
| `/rna-mcp:record` | Record business artifacts (metis, signals, guardrails, outcome updates) with frontmatter templates |

### CLI Subcommands

| Command | What it does |
|---------|-------------|
| `search <query>` | Search symbols by name/signature, filter by kind/language/file |
| `graph --node <id> --mode <mode>` | Traverse neighbors, impact analysis, or reachability |
| `scan --path <dir>` | Full scan + extract + embed + persist |
| `stats --repo <dir>` | Show repo stats from persisted index (no re-scan) |
| `test --repo <dir>` | Run 25 pipeline checks end-to-end |
| `setup --project <dir>` | Bootstrap RNA + OH MCP + skills for a project |

## The `.oh/` Directory

```
.oh/
├── outcomes/        <- what we're optimizing for
├── signals/         <- how we measure progress
├── guardrails/      <- constraints that shape behavior
├── metis/           <- learnings that compound across sessions
├── config.toml      <- scanner excludes, per-project tuning
└── .cache/          <- scan state, embedding index (gitignored)
```


Outcomes declare `files:` patterns linking to code. Commits tag `[outcome:X]` linking to outcomes. These structural links power `outcome_progress`.

`.oh/` is a **cache**, not source of truth. `rm -rf .oh/` loses context but breaks nothing.

## Compared To

RNA uses LSP internally as one enrichment source, fuses it with tree-sitter, embeddings, git history, and business artifacts into a cross-language graph, and exposes multi-hop traversal in a single call. For agents, RNA replaces the need for separate LSP plugins. See the [full comparison](docs/compared-to.md) for details (including LSP as the baseline).

| | **LSP (baseline)** | **RNA** | **Code-Graph-RAG** | **CodeGraphContext** |
|---|---|---|---|---|
| **What it is** | Editor protocol | Aim-conditioned MCP server | Code RAG system | Code graph toolkit + MCP |
| **Query model** | 1 symbol, 1 hop, 1 language | Multi-hop, cross-language | Multi-hop | Multi-hop |
| **Install** | Editor plugin / PATH binary | Single binary | Docker + Memgraph + API key | pip + graph DB |
| **External deps** | One server per language | None | Docker, Memgraph, LLM API | Graph DB (KuzuDB/Neo4j) |
| **Languages** | 1 per server | 22 (tree-sitter) + 37 (LSP) | 11 (tree-sitter) | 14 (tree-sitter) |
| **Embeddings** | None | MiniLM-L6-v2 on Metal GPU | UniXcoder | None |
| **Business context** | None | Outcomes, signals, guardrails, metis | None | None |

LSP is the baseline — what agents get if you install nothing else. It provides single-symbol, single-hop queries; agents must make N sequential round-trips and assemble the picture themselves. Early testing: ~120s and ~2x tokens with raw LSP vs ~50s with RNA for the same structural questions.

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

## Status

**Working:**

- Function-level semantic search — embeds function bodies (not just names), all markdown, and commits in one vector space. Searches by meaning, scored 0-1 with production code ranked above tests
- Structural outcome-to-code joins — agents scope work to declared business outcomes without re-prompting
- Metal GPU semantic search with adaptive batch sizing — self-tunes to hardware
- LSP-enriched call/type graph — compiler-grade call hierarchy and Implements edges across 37 language servers
- Semantic graph entry points — `graph_query` accepts natural language queries directly, no node_id lookup required
- Query staleness awareness — footers show `LSP: pending` so agents know when results are incomplete
- Event-driven reindex — background scanner detects changes, re-extracts, re-embeds without blocking queries
- Persistent index with <1s restarts — LanceDB cache survives process restarts
- 22 language extractors, cross-language constant search
- 5 MCP tools, 6 CLI subcommands, 370+ tests
- Ships as a Claude Code plugin with setup skill and record skill

### Platform Support

| Platform | Status | Embeddings |
|----------|--------|------------|
| macOS Apple Silicon (ARM) | Full support | Metal GPU (fast) |
| Linux x86_64 | Supported | CPU-only (slower semantic search) |
| Windows | Untested | — |

### Tested On

| Harness | Repo types |
|---------|-----------|
| Claude Code | Rust, Python/TypeScript monorepo, Rust/TypeScript |
| Oh-My-Pi | Rust, Python/TypeScript monorepo |

Only tested on Apple Silicon (M-series) Macs. Linux x86_64 builds are available but less battle-tested.

## License

MIT — see [LICENSE](LICENSE).

## Glossary

| Term | What it means |
|------|--------------|
| **Tree-sitter** | A parser that reads source code and produces a syntax tree — the structured representation of functions, classes, imports, etc. RNA uses it to extract symbols from 22 languages without running the code. |
| **LSP** | Language Server Protocol. The same protocol your editor uses for go-to-definition and find-references. LSP provides single-symbol, single-hop, single-language queries — no multi-hop traversal, no cross-language, no semantic search. RNA runs LSP servers internally as one enrichment source (call hierarchy, type hierarchy, implements edges) and fuses the results into a unified cross-language graph. For agents, RNA replaces the need for separate LSP plugins. |
| **Graph** | A network of nodes (symbols, files, outcomes) and edges (calls, depends_on, implements). RNA builds this in memory so you can ask "what depends on this function?" or "what's the blast radius of this change?" |
| **Embeddings** | Vector representations of text that capture meaning. RNA embeds function bodies, markdown sections, commit messages, and business artifacts so `oh_search_context` can find relevant results by meaning, not just keywords. Uses Metal GPU on Apple Silicon, CPU elsewhere. |
| **LanceDB** | The columnar + vector database RNA uses to store the graph and embeddings on disk. Lives in `.oh/.cache/`. |
| **petgraph** | The in-memory graph index RNA uses for fast traversal (neighbors, impact analysis, reachability). Rebuilt from LanceDB on startup. |
| **Outcome** | A business result you're optimizing for. Example: "Agents correctly scope work to declared aims." |
| **Signal** | How you measure progress toward an outcome. Example: "Agent identifies which outcome a task serves without re-prompting." |
| **Guardrail** | A constraint that shapes behavior — hard (never bend), soft (negotiate), or candidate (proposed). Example: "No language-specific conditionals in generic.rs." |
| **Metis** | A learning earned through experience — [Greek: practical wisdom](https://en.wikipedia.org/wiki/Metis_(mythology)) gained from doing, not reading. Example: "Protocol version mismatch silently hangs MCP clients." |
| **MCP** | Model Context Protocol. The standard for connecting AI agents to external tools. RNA exposes its capabilities as MCP tools that Claude Code (and other MCP clients) can call. |

## Detailed Documentation

- [Compared To](docs/compared-to.md) — RNA vs Code-Graph-RAG, CodeGraphContext
- [Extractors](docs/extractors.md) — 22 language extractors, constants, synthetic literals
- [LSP Enrichment](docs/lsp-enrichment.md) — 37 auto-detected language servers
- [Scanner](docs/scanner.md) — incremental, event-driven, worktree-aware scanning
- [Graph Architecture](docs/graph.md) — LanceDB + petgraph, edge types, SourceEnvelope
- [Source Compatibility](docs/rna-source-compatibility.md) — source-capability design for future Context Assembler integration
