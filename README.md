# Repo-Native Alignment

[![CI](https://github.com/open-horizon-labs/repo-native-alignment/actions/workflows/rust-main-merge.yml/badge.svg)](https://github.com/open-horizon-labs/repo-native-alignment/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Local context discovery and alignment tool for coding agents. Makes the fractal, local knowledge in your codebase — architecture, topology, business intent, the stuff not in training data — discoverable and queryable. Single binary — no Docker, no external database, no API key.

**[Quick Start](#quick-start)** | **[What Agents Can Do](#what-agents-can-do)** | **[RNA vs LSP](#why-not-just-lsp)** | **[MCP Tools](#mcp-tools)** | **[Compared To](#compared-to)** | **[Docs](#detailed-documentation)**

## What Agents Can Do

### Find code by meaning, not just by name

- "Find functions related to payment processing" → `search("payment processing")` → ranked results across code symbols, artifacts, commits, and markdown in one call
- "How does scanning work?" → `search("scanning")` → implementation code, doc sections, and related artifacts together
- "Where is the authentication handler?" → `search("AuthHandler")` → file, line, signature, complexity, graph edges
- "Find the validate function in auth/handlers, not billing" → `search("auth/handlers/validate")` → path/name split: only symbols named `validate` in files matching `auth/handlers`
- "What are the riskiest functions?" → `search(query="", min_complexity=20, sort_by="complexity")` → hotspots ranked by cyclomatic complexity
- "What are the most important symbols?" → `search(sort_by="importance")` → top symbols ranked by PageRank
- "Give me a map of this repo" → `repo_map()` → **subsystems** with cohesion scores and interfaces, top symbols, hotspot files, active outcomes, entry points
- "What subsystems exist?" → `repo_map()` → detected from actual call relationships: `extract (1120 symbols)`, `server (721)`, `graph (223)`, ...
- "Find auth code in the server subsystem" → `search(query="auth", subsystem="server")` → scoped to detected subsystem
- "What connects extract to server?" → `search(node="X", mode="neighbors", target_subsystem="server")` → cross-subsystem edges
- "Show me the full structure of a module" → `search(node="server:module", mode="neighbors", depth=2, compact=true)` → module → members → their members in one call

### See the blast radius of a change

- "What depends on the database connection pool?" → `search(query="database connection pool", mode="impact")` → transitive dependents grouped by subsystem with entry points; auto-summarized when large
- "What calls AuthHandler?" → `search(query="AuthHandler", mode="neighbors", direction="incoming")` → callers, implementors
- "Find all trait implementors" → `search(query="Enricher trait", mode="neighbors", edge_types=["implements"])` → concrete types with compiler-grade edges

### Connect code to business outcomes

- "How is the agent-alignment outcome progressing?" → `outcome_progress("agent-alignment")` → tagged commits → changed files → symbols → PRs
- "Find signals related to reliability" → `search("reliability", artifact_types=["signal"])` → measurement definitions
- "What are our constraints?" → `search("constraints guardrails")` → all guardrails ranked by relevance

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
search(query="ConnectionPool", mode="impact", max_hops=3)
→ PoolManager → AppConfig
→ HttpServer → main, Router
→ Worker → Scheduler
// production code ranked first, test files demoted, cross-language
```

| Question | LSP alone | RNA |
|---|---|---|
| What breaks if I change the connection pool? | N round-trips of `incomingCalls`, agent assembles graph | `search(mode="impact")` — one call, transitive |
| Find code related to payment processing | No semantic search — agent must guess names and grep | `search("payment processing")` — ranked by meaning across code, artifacts, and markdown |
| How is our reliability outcome progressing? | Not possible — LSP has no business context | `outcome_progress("reliability")` — commits → files → symbols |

LSP gives agents single-symbol, single-hop, single-language queries. There's no multi-hop primitive. RNA runs those same LSP servers internally, fuses their data with tree-sitter, embedded function bodies, git history, and business artifacts into a cross-language graph.

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
repo-native-alignment scan --repo . --full   # build full index
repo-native-alignment search "auth" --repo /path/to/your/project  # search symbols
repo-native-alignment graph --node "<stable-id-from-search>" --mode impact --repo .
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

### 4. Build the index

```bash
repo-native-alignment scan --repo . --full
```

Runs the complete pipeline with visible output: scan → extract → embed → LSP enrich → graph. Shows timing and edge counts for each phase. Recommended before first MCP session so agents start with a warm index including LSP call edges.

### 5. Verify the pipeline

```bash
repo-native-alignment test --repo /path/to/your/project
```

Runs 25 checks end-to-end. Exits 0 on pass, 1 on failure. Safe to run in CI.

### 6. Start working

The system compounds from here. Agents use `search` to discover relevant context across code, artifacts, commits, and markdown, and write learnings to `.oh/metis/`. Each session starts richer than the last.

## MCP Tools

| Tool | What it's for |
|------|--------------|
| `search` | Code symbols, artifacts, commits, and markdown — flat or graph traversal (`mode`: neighbors, impact, reachable, tests_for). Scope to a subsystem (`subsystem=`), filter cross-subsystem edges (`target_subsystem=`), use `depth=2` with neighbors to walk N levels deep, use `compact: true` for ~25x fewer tokens, `rerank: true` for precision. Short node IDs resolve automatically. |
| `repo_map` | Repository orientation: **detected subsystems** with their key interfaces, top symbols by importance, hotspot files, active outcomes, entry points. One call replaces an exploratory loop. |
| `outcome_progress` | Connect business outcomes to code: outcome → tagged commits → changed files → symbols. Optional `include_impact: true` for risk-classified blast radius. |
| `list_roots` | Show which workspace roots are configured and their scan status |

**Root scoping:** All query tools default to the primary workspace root (`--repo`). Pass `root: "all"` for cross-root search, or `root: "<slug>"` for a specific root. Non-code roots (.oh/ artifacts, commits, Notes) always pass through regardless of root filter.

**Auto-discovered roots:** RNA automatically adds git worktrees and Claude Code memory as additional roots. Use `list_roots` to see what's active.

**Declared roots:** Declare intentionally related repos by slug in `.oh/config.toml`:

```toml
# .oh/config.toml
[scanner]
exclude = ["benchmark/"]

[workspace.roots]
infra   = "../k8s-configs"    # relative to the repo containing .oh/
protos  = "/abs/path/protos"  # absolute paths also work
```

After declaring roots, restart RNA. Declared roots appear in `list_roots()` and are queryable by slug:

```
list_roots()                          # shows "infra", "protos", primary, worktrees
search(root="infra", query="Deployment")  # only K8s manifest results
search(root="all")                    # all declared + auto-discovered roots
```

Missing paths warn and skip (not an error). Relative paths are resolved relative to the repo containing `.oh/config.toml`.

### CLI ↔ MCP Equivalence

CLI and MCP share the same index. Run `scan --full` from the CLI to build the complete index (including call graph edges from your language server), then query via either interface. A pre-built index means the MCP server starts with warm data — no cold-start delay.

```bash
# Build the full index (visible, verifiable)
rna scan --repo . --full

# Then query via CLI...
rna search "auth" --repo .
rna graph --node "<id>" --mode impact --repo .

# ...or via MCP (same data, same results)
search(query="auth")
search(node="<id>", mode="impact")
```

| CLI | MCP | What it does |
|-----|-----|-------------|
| `search "auth"` | `search(query="auth")` | Find symbols by name |
| `graph --node <id> --mode neighbors` | `search(node="<id>", mode="neighbors")` | Graph traversal |
| `scan --full` | *(runs automatically on first query)* | Full pipeline: scan → extract → embed → LSP → graph |
| `test` | — | 29 pipeline checks end-to-end |

### Building the Index

The MCP server builds an index automatically on first query. For best results — including subsystem detection and impact analysis — build the full index from the CLI first so language server analysis runs before agents start:

```bash
repo-native-alignment scan --repo . --full

# Scan+Extract: 8,700 symbols across 210 files in 0.6s
# Embed: 2,800 items in 30s
# LSP: 8,200 call edges in 50s
# Done in 50s
```

Without `--full`, the scan skips language server analysis — subsystem detection and "what calls this" queries won't work.

**After upgrading RNA**, clear the old index and rebuild:

```bash
rm -rf .oh/.cache/lance .oh/.cache/scan-state.json
repo-native-alignment scan --repo . --full
```

### CLI Subcommands

| Command | What it does |
|---------|-------------|
| `search <query>` | Search symbols by name, keyword, or meaning — filter by kind/language/file |
| `graph --node <id> --mode <mode>` | Traverse neighbors, impact analysis, or reachability |
| `scan --repo <dir>` | Scan + extract + embed + persist |
| `scan --repo <dir> --full` | Full pipeline including LSP enrichment. Incremental when cache exists (~0.1s on no-change runs). LSP aborts early if misconfigured (0 edges after 1,000 nodes or 2 minutes). |
| `stats --repo <dir>` | Show repo stats from persisted index (no re-scan) |
| `test --repo <dir>` | Run 29 pipeline checks end-to-end |
| `setup --project <dir>` | Bootstrap RNA + OH MCP + skills for a project |

### Plugin Skills

| Skill | What it does |
|-------|-------------|
| `/rna-mcp:setup` | Download binary, configure MCP, update AGENTS.md |
| `/rna-mcp:record` | Record business artifacts (metis, signals, guardrails, outcome updates) with frontmatter templates |

## The `.oh/` Directory

```
.oh/
├── outcomes/        <- what we're optimizing for
├── signals/         <- how we measure progress
├── guardrails/      <- constraints that shape behavior
├── metis/           <- learnings that compound across sessions
├── config.toml      <- scanner excludes, pattern detection, declared workspace roots
└── .cache/          <- scan state, embedding index (gitignored)
```

Business artifacts (`outcomes/`, `signals/`, `guardrails/`, `metis/`) are committed to git — they're part of the project. `.cache/` is gitignored and rebuilt automatically on first query.

RNA also indexes agent rule/memory files when they exist alongside a project:

| Path pattern | `artifact_types` filter |
|---|---|
| `.cursorrules`, `.cursor/**` | `cursor-rule` |
| `.clinerules` (file) | `cline-rule` |
| `.serena/memories/**` | `serena-memory` |
| `.github/copilot-instructions.md` | `copilot-instruction` |

These are auto-detected — no configuration needed. Use `search("coding rules", artifact_types=["cursor-rule", "cline-rule"])` to query across all agent rule sources.

Outcomes declare `files:` patterns linking to code. Commits tag `[outcome:X]` linking to outcomes. These structural links power `outcome_progress`.

## Compared To

See the [full comparison](docs/compared-to.md) for details, including LSP as the baseline.

| | **RNA** | **Code-Graph-RAG** | **CodeGraphContext** | **Serena** |
|---|---|---|---|---|
| **Install** | Single binary | Docker + Memgraph + API key | pip + graph DB | `pip install mcp-server-serena` |
| **External deps** | None | Docker, Memgraph, LLM API | Graph DB (KuzuDB/Neo4j) | None (language servers auto-downloaded) |
| **Languages** | Tree-sitter + LSP | Tree-sitter only | Tree-sitter only | 30+ via LSP |
| **Embeddings** | MiniLM-L6-v2 on Metal GPU | UniXcoder | None | None |
| **Business context** | Outcomes, signals, guardrails, metis | None | None | Agent memories (auto-accumulated, not curated outcomes) |

## Optional: Companion Systems

RNA works standalone. These add organizational context and workflow structure:

- **[OH MCP](https://github.com/cloud-atlas-ai/oh-mcp-server)** — cross-project context: missions, aims, endeavors, decision logs
- **[OH Skills](https://github.com/open-horizon-labs/skills)** — workflow skills: `/aim`, `/review`, `/dissent`, `/salvage`, `/solution-space`, `/execute`

## Status

4 MCP tools, 10 CLI subcommands. Extracts symbols from 22 languages, builds a call graph via language server analysis, detects architectural subsystems automatically. Ships as a Claude Code plugin. CLI and MCP share the same index and service layer.

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
| **Embeddings** | Vector representations of text that capture meaning. RNA embeds function bodies, markdown sections, commit messages, and business artifacts so `search` can find relevant results by meaning, not just keywords. Uses Metal GPU on Apple Silicon, CPU elsewhere. |
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
- [Graph Architecture](docs/graph.md) — edge types, persistence, in-memory index
- [Source Compatibility](docs/rna-source-compatibility.md) — source-capability design for future Context Assembler integration
