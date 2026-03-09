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

## Quick Start

### Prerequisites

| Dependency | Install | Required? |
|------------|---------|-----------|
| `cargo` | [rustup.rs](https://rustup.rs) | Yes |
| `npx` | ships with Node.js — [nodejs.org](https://nodejs.org) | Only for `setup --project` (installs OH Skills) |

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

Runs 22+ checks end-to-end. Exits 0 on pass, 1 on failure. Safe to run in CI.

### 5. Start working

The system compounds from here. Agents use `oh_search_context` to discover relevant context, `search_symbols` to explore code, and `oh_record` to capture learnings. Each session starts richer than the last.

---

**Manual path:** Build from source (`cargo build --release`), add `rna-server` to your `.mcp.json` pointing at the binary with `--repo <project path>`. The setup command automates this.

## RNA MCP Server — 9 tools

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

**Working today:** 9 MCP tools, 22 language extractors, 190+ tests. Structural outcome-to-code joins, LSP-enriched impact analysis, cross-language constant search, Metal GPU semantic search, event-driven reindex, persistent index with <1s restarts. Validated on 3 repos.

## Detailed Documentation

- [Extractors](docs/extractors.md) — 22 language extractors, constants, synthetic literals
- [LSP Enrichment](docs/lsp-enrichment.md) — 37 auto-detected language servers
- [Scanner](docs/scanner.md) — incremental, event-driven, worktree-aware scanning
- [Graph Architecture](docs/graph.md) — LanceDB + petgraph, edge types, SourceEnvelope
- [Source Compatibility](docs/rna-source-compatibility.md) — source-capability design for future Context Assembler integration
