# Repo-Native Alignment

[![CI](https://github.com/open-horizon-labs/repo-native-alignment/actions/workflows/rust-main-merge.yml/badge.svg)](https://github.com/open-horizon-labs/repo-native-alignment/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Aim-conditioned decision infrastructure for coding agents. Agents don't just execute — they plan and adapt conditioned on declared aims, treating repo artifacts as evidence that updates confidence in whether the current aim framing is still correct.

We don't build features, we build capabilities.

## Platform Support

| Platform | Status | Embeddings |
|----------|--------|------------|
| macOS Apple Silicon (ARM) | Full support | Metal GPU (fast) |
| Linux x86_64 | Supported | CPU-only (slower semantic search) |
| Windows | Untested | — |

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

Setup detects your platform (M4 optimized binary for M4+ chips), downloads the binary to `~/.cargo/bin/`, configures `.mcp.json`, and updates AGENTS.md with tool guidance.

**Download a prebuilt binary** (manual):

```bash
mkdir -p ~/.cargo/bin

# macOS Apple Silicon (M4+)
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-darwin-arm64-m4 -o ~/.cargo/bin/repo-native-alignment && chmod +x ~/.cargo/bin/repo-native-alignment

# macOS Apple Silicon (M1/M2/M3)
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-darwin-arm64 -o ~/.cargo/bin/repo-native-alignment && chmod +x ~/.cargo/bin/repo-native-alignment

# Linux x86_64
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-linux-x86_64 -o ~/.cargo/bin/repo-native-alignment && chmod +x ~/.cargo/bin/repo-native-alignment
```

**Build from source** (requires [Rust toolchain](https://rustup.rs)):

```bash
git clone https://github.com/open-horizon-labs/repo-native-alignment.git
cd repo-native-alignment
cargo install --locked --path .
```

### 1b. Try it from the CLI

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

Install the skills framework, then open a Claude Code session in your project:

```bash
# Add the skills marketplace
claude plugin marketplace add open-horizon-labs/skills

# Install OH Skills
claude plugin install oh-skills
```

Then run:

```
/oh-skills:teach-oh
```

This explores your codebase, asks about your aims, writes `AGENTS.md`, scaffolds `.oh/` with outcomes and constraints, and installs phase agents. RNA tools are automatically detected and used during exploration if installed.

### 4. Verify the pipeline

```bash
repo-native-alignment test --repo /path/to/your/project
```

Runs 22+ checks end-to-end. Exits 0 on pass, 1 on failure. Safe to run in CI.

### 5. Start working

The system compounds from here. Agents use `oh_search_context` to discover relevant context, `search_symbols` to explore code, and write learnings to `.oh/metis/`. Each session starts richer than the last.

## RNA MCP Server — 7 tools

| Category | Tools |
|----------|-------|
| **Context** | `oh_get_context` — all business artifacts in one call |
| **Search** | `oh_search_context` — semantic search over .oh/ + commits (optionally code + markdown) |
| **Code** | `search_symbols` — multi-language symbol search with graph edges |
| **Graph** | `graph_query` — traverse neighbors, impact analysis, reachability |
| **History** | `oh_search_context` returns commit hash — use `git show <hash>` via Bash for diffs |
| **Join** | `outcome_progress` — structural join: outcome → commits → symbols → PRs |
| **Workspace** | `list_roots` — show configured workspace roots |

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
| `test --repo <dir>` | Run 22+ pipeline checks end-to-end |
| `setup --project <dir>` | Bootstrap RNA + OH MCP + skills for a project |

## The `.oh/` Directory

```
.oh/
├── outcomes/        <- what we're optimizing for (YAML frontmatter + markdown)
├── signals/         <- how we measure progress (SLO definitions + observations)
├── guardrails/      <- constraints that shape behavior (hard/soft/candidate)
├── metis/           <- learnings that compound across sessions
├── config.toml      <- scanner excludes, per-project tuning
└── .cache/          <- scan state, embedding index (gitignored)
```


Outcomes declare `files:` patterns linking to code. Commits tag `[outcome:X]` linking to outcomes. These structural links power `outcome_progress`.

`.oh/` is a **cache**, not source of truth. `rm -rf .oh/` loses context but breaks nothing.

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

**Working today:** 7 MCP tools, 6 CLI subcommands, 22 language extractors, 190+ tests. Structural outcome-to-code joins, LSP-enriched impact analysis, cross-language constant search, Metal GPU semantic search (CPU fallback on Linux), event-driven reindex, persistent index with <1s restarts. Ships as a Claude Code plugin with setup skill and record skill.

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
| **LSP** | Language Server Protocol. The same protocol your editor uses for go-to-definition and find-references. RNA talks to language servers to enrich the graph with type information and call hierarchies. |
| **Graph** | A network of nodes (symbols, files, outcomes) and edges (calls, depends_on, implements). RNA builds this in memory so you can ask "what depends on this function?" or "what's the blast radius of this change?" |
| **Embeddings** | Vector representations of text that capture meaning. RNA embeds code signatures, commit messages, and business artifacts so `oh_search_context` can find relevant results by meaning, not just keywords. Uses Metal GPU on Apple Silicon, CPU elsewhere. |
| **LanceDB** | The columnar + vector database RNA uses to store the graph and embeddings on disk. Lives in `.oh/.cache/`. |
| **petgraph** | The in-memory graph index RNA uses for fast traversal (neighbors, impact analysis, reachability). Rebuilt from LanceDB on startup. |
| **Outcome** | A business result you're optimizing for. Example: "Agents correctly scope work to declared aims." |
| **Signal** | How you measure progress toward an outcome. Example: "Agent identifies which outcome a task serves without re-prompting." |
| **Guardrail** | A constraint that shapes behavior — hard (never bend), soft (negotiate), or candidate (proposed). Example: "No language-specific conditionals in generic.rs." |
| **Metis** | A learning earned through experience — [Greek: practical wisdom](https://en.wikipedia.org/wiki/Metis_(mythology)) gained from doing, not reading. Example: "Protocol version mismatch silently hangs MCP clients." |
| **MCP** | Model Context Protocol. The standard for connecting AI agents to external tools. RNA exposes its capabilities as MCP tools that Claude Code (and other MCP clients) can call. |

## Detailed Documentation

- [Extractors](docs/extractors.md) — 22 language extractors, constants, synthetic literals
- [LSP Enrichment](docs/lsp-enrichment.md) — 37 auto-detected language servers
- [Scanner](docs/scanner.md) — incremental, event-driven, worktree-aware scanning
- [Graph Architecture](docs/graph.md) — LanceDB + petgraph, edge types, SourceEnvelope
- [Source Compatibility](docs/rna-source-compatibility.md) — source-capability design for future Context Assembler integration
