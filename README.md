# Repo-Native Alignment

[![CI](https://github.com/open-horizon-labs/repo-native-alignment/actions/workflows/rust-main-merge.yml/badge.svg)](https://github.com/open-horizon-labs/repo-native-alignment/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Coding agents are blind to the shape of your codebase. RNA fixes that: it runs as an MCP server alongside your agent and gives it one call to answer "what depends on this?", "what relates to payment processing?", or "how is the reliability outcome progressing?" — questions LSP alone cannot answer.

Single binary. No Docker. No external database. No API key.

**[Quick Start](#quick-start)** | **[Why RNA](#why-rna)** | **[MCP Tools](#mcp-tools)** | **[Docs](#detailed-documentation)**

---

## Why RNA

LSP gives agents single-symbol, single-hop, single-language queries. There's no multi-hop primitive, no semantic search, no connection to business artifacts.

**Finding blast radius with LSP alone:**
```
textDocument/references("ConnectionPool")     → [PoolManager, HttpServer, Worker]
callHierarchy/incomingCalls(PoolManager)      → [AppConfig, TestHarness]
callHierarchy/incomingCalls(HttpServer)       → [main, Router]
callHierarchy/incomingCalls(Worker)           → [Scheduler]
// agent must: filter test files, deduplicate, reason about the shape
```

**With RNA:**
```
search(query="ConnectionPool", mode="impact", max_hops=3)
→ PoolManager → AppConfig
→ HttpServer → main, Router
→ Worker → Scheduler
// production code ranked first, test files demoted, cross-language
```

| Job | LSP alone | RNA |
|---|---|---|
| What breaks if I change this? | N round-trips of `incomingCalls`, agent assembles graph | `search(mode="impact")` — one call, transitive |
| Find code related to a concept | No semantic search — agent must guess names and grep | `search("payment processing")` — ranked by meaning across code, docs, and artifacts |
| How is our reliability outcome progressing? | Not possible — LSP has no business context | `outcome_progress("reliability")` — commits → files → symbols |
| Orient a new agent to the repo | Multiple searches, no subsystem picture | `repo_map()` — subsystems, hotspots, entry points in one call |

RNA runs LSP servers internally. It fuses their data with tree-sitter, embedded function bodies, git history, and business artifacts into a cross-language graph — so the agent gets the LSP depth without doing the LSP orchestration.

## What Changes After Installing

Four jobs agents can do after RNA is running that they could not do reliably before:

**Find code by meaning, not just by name**
`search("payment processing")` returns ranked results across symbols, docs, commits, and artifacts in one call. Path scoping works too: `search("auth/handlers/validate")` returns only symbols named `validate` in files matching `auth/handlers`.

**Trace call paths and blast radius**
`search(node="AuthHandler", mode="impact")` returns transitive dependents grouped by subsystem. `search(node="X", mode="path", query="Y")` returns the directed call chain between two nodes.

**Connect code to business outcomes**
`outcome_progress("agent-alignment")` follows tagged commits to changed files to affected symbols. Outcomes, signals, and guardrails in `.oh/` are full graph nodes — searchable, linkable, tracked.

**Orient instantly in an unfamiliar repo**
`repo_map()` returns detected subsystems (from actual call relationships), top symbols by PageRank, hotspot files, and active outcomes. One call instead of an exploratory loop.

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

### 2. Connect to your MCP client

The MCP server command is `repo-native-alignment` with `--repo` as an argument. `command` must be just the binary name — the `--repo` flag goes in `args`, not in `command` (MCP stdio transport doesn't do shell splitting).

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

### 3. Build the index

```bash
repo-native-alignment scan --repo . --full
```

Runs the complete pipeline: scan → extract → embed → LSP enrich → graph. Without `--full`, LSP analysis is skipped — subsystem detection and "what calls this" queries won't work. Subsequent scans are incremental (~0.1s on no-change runs).

### 4. Verify

```bash
repo-native-alignment test --repo /path/to/your/project
```

Runs 29 checks end-to-end. Exits 0 on pass, 1 on failure. Safe to run in CI.

### 5. Try it from the CLI

Before wiring up MCP, evaluate RNA directly:

```bash
repo-native-alignment search "auth" --repo /path/to/your/project
repo-native-alignment graph --node "<stable-id-from-search>" --mode impact --repo .
```

### 6. Teach your agents (optional — requires [OH Skills](https://github.com/open-horizon-labs/skills))

Install OH Skills ([see instructions](https://github.com/open-horizon-labs/skills#install)), then open a Claude Code session in your project and run:

```
/teach-oh
```

This explores your codebase, asks about your aims, writes `AGENTS.md`, scaffolds `.oh/` with outcomes and constraints, and installs phase agents.

## MCP Tools

| Tool | What it's for |
|------|--------------|
| `search` | Code symbols, artifacts, commits, and markdown — flat or graph traversal (`mode`: neighbors, impact, reachable, tests_for, cycles, path). Scope to a subsystem (`subsystem=`), filter cross-subsystem edges (`target_subsystem=`), use `compact: true` for ~25x fewer tokens, `rerank: true` for precision. |
| `repo_map` | Repository orientation: detected subsystems with their key interfaces, top symbols by importance, hotspot files, active outcomes, entry points. One call replaces an exploratory loop. |
| `outcome_progress` | Connect business outcomes to code: outcome → tagged commits → changed files → symbols. Optional `include_impact: true` for risk-classified blast radius. |
| `list_roots` | Show configured workspace roots with live scan stats (symbols, edges, detected frameworks, LSP edge counts per language, scan phase). Includes LSP servers available to install for each root's detected languages. |

**Root scoping:** All query tools default to the primary workspace root (`--repo`). Pass `root: "all"` for cross-root search, or `root: "<slug>"` for a specific root.

**Worktree-aware queries:** Agents working in a git worktree can query their own code by passing the absolute path: `search(query="...", repo="/absolute/path/to/worktree")`. The worktree must be scanned first.

### CLI ↔ MCP Equivalence

CLI and MCP share the same index. Run `scan --full` from the CLI to build the complete index, then query via either interface.

| CLI | MCP | What it does |
|-----|-----|-------------|
| `search "auth"` | `search(query="auth")` | Find symbols by name |
| `graph --node <id> --mode neighbors` | `search(node="<id>", mode="neighbors")` | Graph traversal |
| `scan --full` | *(runs automatically on first query)* | Full pipeline: scan → extract → embed → LSP → graph |
| `test` | — | 29 pipeline checks end-to-end |

### CLI Subcommands

| Command | What it does |
|---------|-------------|
| `search <query>` | Search symbols by name, keyword, or meaning — filter by kind/language/file |
| `graph --node <id> --mode <mode>` | Traverse neighbors, impact analysis, or reachability |
| `scan --repo <dir>` | Scan + extract + embed + persist |
| `scan --repo <dir> --full` | Full pipeline including LSP enrichment. Incremental on repeat runs. |
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
├── config.toml      <- scanner excludes, LSP severity threshold, declared workspace roots
├── extractors/      <- custom boundary detection configs (optional, see below)
└── .cache/          <- scan state, embedding index (gitignored)
```

Business artifacts (`outcomes/`, `signals/`, `guardrails/`, `metis/`) are committed to git. `.cache/` is gitignored and rebuilt automatically on first query.

### Workspace roots

Declare intentionally related repos in `.oh/config.toml`:

```toml
[scanner]
exclude = ["benchmark/"]

[lsp]
# Minimum severity to store as diagnostic nodes.
# "error" | "warning" (default) | "information" | "hint"
diagnostic_min_severity = "hint"

[workspace.roots]
infra   = "../k8s-configs"
protos  = "/abs/path/protos"
```

After declaring roots, run `scan` (or restart RNA). Roots appear in `list_roots()` and are queryable by slug:

```text
search(root="infra", query="Deployment")  # only K8s results
search(root="all")                        # all roots
```

### Custom boundary detection

Declare custom pub/sub or event-bus boundary patterns in `.oh/extractors/*.toml`. RNA reads these at scan time and emits `Produces`/`Consumes` edges without any changes to RNA source.

```toml
# .oh/extractors/internal-event-bus.toml
[meta]
name = "internal-event-bus"
applies_when = { language = "python", imports_contain = "src.events.bus" }

[[boundaries]]
function_pattern = "bus.publish"
arg_position = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "bus.subscribe"
arg_position = 0
edge_kind = "Consumes"
```

Fields: `applies_when.language`, `applies_when.imports_contain`, `function_pattern` (substring or glob), `arg_position` (zero-indexed string literal argument holding the topic), `edge_kind` (`"Produces"` or `"Consumes"`), `decorator` (set `true` when matching a decorator name).

RNA includes built-in extractors for kafka-python, kafkajs, celery, pika, and redis-py. Use `.oh/extractors/` for any other broker or custom RPC framework.

RNA also indexes agent rule/memory files automatically:

| Path pattern | `artifact_types` filter |
|---|---|
| `.cursorrules`, `.cursor/**` | `cursor-rule` |
| `.clinerules` (file) | `cline-rule` |
| `.serena/memories/**` | `serena-memory` |
| `.github/copilot-instructions.md` | `copilot-instruction` |

## Compared To

See the [full comparison](docs/compared-to.md) for details.

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

4 MCP tools, 10 CLI subcommands. Extracts symbols from 22 languages, builds a call graph via language server analysis, detects architectural subsystems and frameworks automatically.

**v0.1.15 (current):** EventBus/consumer architecture, parallel LSP enrichment, config-driven extractors (`.oh/extractors/*.toml`), live scan stats in `list_roots`, worktree own-cache detection, FastAPI router prefix resolution.

### Platform Support

| Platform | Status | Embeddings |
|----------|--------|------------|
| macOS Apple Silicon (ARM) | Full support | Metal GPU (fast) |
| Linux x86_64 | Supported | CPU-only (slower semantic search) |
| Windows | Untested | — |

## License

MIT — see [LICENSE](LICENSE).

## Detailed Documentation

- [Compared To](docs/compared-to.md) — RNA vs Code-Graph-RAG, CodeGraphContext
- [Extractors](docs/extractors.md) — tree-sitter language extractors, constants, synthetic literals
- [LSP Enrichment](docs/lsp-enrichment.md) — auto-detected language servers
- [Scanner](docs/scanner.md) — incremental, event-driven, worktree-aware scanning
- [Graph Architecture](docs/graph.md) — edge types, persistence, in-memory index
- [Source Compatibility](docs/rna-source-compatibility.md) — source-capability design for future Context Assembler integration
