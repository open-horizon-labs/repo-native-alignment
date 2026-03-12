# Repo-Native Alignment

[![CI](https://github.com/open-horizon-labs/repo-native-alignment/actions/workflows/rust-main-merge.yml/badge.svg)](https://github.com/open-horizon-labs/repo-native-alignment/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

An MCP server that gives coding agents what LSP alone can't: a cross-language code graph, semantic search, and business outcome tracking. Single binary — no Docker, no external database, no API key.

**[Quick Start](#quick-start)** | **[What Agents Can Do](#what-agents-can-do)** | **[RNA vs LSP](#why-not-just-lsp)** | **[MCP Tools](#mcp-tools)** | **[Compared To](#compared-to)** | **[Docs](#detailed-documentation)**

## What Agents Can Do

### Find code by meaning, not just by name

- "Find functions related to payment processing" → `oh_search_context("payment processing", include_code=true)` → ranked results, production code before tests, searched by meaning across function bodies, docs, and commit history
- "How does scanning work?" → `oh_search_context("scanning", include_code=true, include_markdown=true)` → implementation code and doc sections together
- "Where is the authentication handler?" → `search_symbols("AuthHandler")` → file, line, signature, graph edges

### See the blast radius of a change

- "What depends on the database connection pool?" → `graph_query(query="database connection pool", mode="impact")` → transitive dependents across languages, one call
- "What calls AuthHandler?" → `graph_query(query="AuthHandler", direction="incoming")` → callers, implementors
- "Find all trait implementors" → `graph_query(query="Enricher trait", edge_types=["implements"])` → concrete types with compiler-grade edges

### Connect code to business outcomes

- "How is the agent-alignment outcome progressing?" → `outcome_progress("agent-alignment")` → tagged commits → changed files → symbols → PRs
- "Find signals related to reliability" → `oh_search_context("reliability", artifact_types=["signal"])` → measurement definitions
- "What are our constraints?" → `oh_search_context("constraints guardrails")` → all guardrails ranked by relevance

## Why Not Just LSP?

**LSP: 4 requests to find who depends on `ConnectionPool`**
```
textDocument/references("ConnectionPool")     → [PoolManager, HttpServer, Worker]
callHierarchy/incomingCalls(PoolManager)      → [AppConfig, TestHarness]
callHierarchy/incomingCalls(HttpServer)       → [main, Router]
callHierarchy/incomingCalls(Worker)           → [Scheduler]
// agent must: filter test files, deduplicate, reason about the shape
```

**RNA: 1 request**
```
graph_query(query="ConnectionPool", mode="impact", max_hops=3)
→ PoolManager → AppConfig
→ HttpServer → main, Router
→ Worker → Scheduler
// production code ranked first, test files demoted, cross-language
```

| Question | LSP alone | RNA |
|---|---|---|
| What breaks if I change the connection pool? | N round-trips of `incomingCalls`, agent assembles graph | `graph_query(mode="impact")` — one call, transitive |
| Find code related to payment processing | No semantic search — agent must guess names and grep | `oh_search_context(include_code=true)` — ranked by meaning |
| How is our reliability outcome progressing? | Not possible — LSP has no business context | `outcome_progress("reliability")` — commits → files → symbols |

LSP gives agents single-symbol, single-hop, single-language queries. There's no multi-hop primitive. RNA runs those same LSP servers internally, fuses their data with tree-sitter, embedded function bodies, git history, and business artifacts into a cross-language graph. Early testing: ~50s and ~half the tokens vs ~120s for the same structural questions with LSP alone.

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

## MCP Tools

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

Business artifacts (`outcomes/`, `signals/`, `guardrails/`, `metis/`) are committed to git — they're part of the project. `.cache/` is gitignored and rebuilt automatically on first query.

Outcomes declare `files:` patterns linking to code. Commits tag `[outcome:X]` linking to outcomes. These structural links power `outcome_progress`.

## Compared To

See the [full comparison](docs/compared-to.md) for details, including LSP as the baseline.

| | **RNA** | **Code-Graph-RAG** | **CodeGraphContext** |
|---|---|---|---|
| **Install** | Single binary | Docker + Memgraph + API key | pip + graph DB |
| **External deps** | None | Docker, Memgraph, LLM API | Graph DB (KuzuDB/Neo4j) |
| **Languages** | Tree-sitter + LSP | Tree-sitter only | Tree-sitter only |
| **Embeddings** | MiniLM-L6-v2 on Metal GPU | UniXcoder | None |
| **Business context** | Outcomes, signals, guardrails, metis | None | None |

## Optional: Companion Systems

RNA works standalone. These add organizational context and workflow structure:

- **[OH MCP](https://github.com/cloud-atlas-ai/oh-mcp-server)** — cross-project context: missions, aims, endeavors, decision logs
- **[OH Skills](https://github.com/open-horizon-labs/skills)** — workflow skills: `/aim`, `/review`, `/dissent`, `/salvage`, `/solution-space`, `/execute`

## Status

Tree-sitter + LSP enrichment, 5 MCP tools, 6 CLI subcommands, 370+ tests. Ships as a Claude Code plugin.

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
| **Tree-sitter** | A parser that reads source code and produces a syntax tree — the structured representation of functions, classes, imports, etc. RNA uses it to extract symbols across languages without running the code. |
| **LSP** | Language Server Protocol — the same protocol your editor uses for go-to-definition and find-references. RNA runs LSP servers internally and builds on their data. See [Why Not Just LSP?](#why-not-just-lsp) |
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
- [Extractors](docs/extractors.md) — tree-sitter language extractors, constants, synthetic literals
- [LSP Enrichment](docs/lsp-enrichment.md) — auto-detected language servers
- [Scanner](docs/scanner.md) — incremental, event-driven, worktree-aware scanning
- [Graph Architecture](docs/graph.md) — LanceDB + petgraph, edge types, SourceEnvelope
- [Source Compatibility](docs/rna-source-compatibility.md) — source-capability design for future Context Assembler integration
