# Repo-Native Alignment

Agents stay aligned to declared business outcomes — not just code correctness — because outcomes, constraints, signals, and learnings live *in the repo* as queryable, evolving artifacts.

## How It Works

Four systems collaborate. Each is independent; together they compound.

```
┌─────────────────────────────────────────────────────────────┐
│  OH MCP (organizational)        RNA MCP (repo-local)        │
│  ─ aims, missions, endeavors    ─ outcomes, signals, code   │
│  ─ cross-project context        ─ structural joins (git+ts) │
│  ─ decision logs                ─ .oh/ read/write           │
│                                 ─ semantic search over .oh/  │
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
└─────────────────────────────────────────────────────────────┘
```

**The loop:** Skills/agents guide the workflow → MCP tools read/write structured context → `.oh/` accumulates learnings → git versions everything → next session starts richer.

**The join:** `outcome_progress` connects layers structurally — outcome → file patterns → tagged commits → code symbols → related markdown. Not keyword matching; structural links.

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

The system compounds from here. Skills and agents use `oh_search_context` to discover relevant context. Write tools record what you learn. Next session starts richer.

---

**Manual path (fallback):** If you prefer full control, build from source (`cargo build --release`), then hand-edit your project's `.mcp.json` to add `rna-server` pointing at the compiled binary with `--repo <project path>`. The setup command exists to make this deterministic and verifiable, not to replace understanding of what it configures.

## The Four Systems

### RNA MCP Server (this repo) — 17 tools

The repo-local intelligence layer. Parses code (tree-sitter), markdown (pulldown-cmark), and git history (git2). Embeds `.oh/` artifacts for semantic search (fastembed-rs + LanceDB). Exposes everything via MCP.

| Category | Tools |
|----------|-------|
| **Read .oh/** | `oh_get_outcomes`, `oh_get_signals`, `oh_get_guardrails`, `oh_get_metis`, `oh_get_context` |
| **Write .oh/** | `oh_record_metis`, `oh_record_signal`, `oh_update_outcome`, `oh_record_guardrail_candidate`, `oh_init` |
| **Search** | `search_markdown`, `search_code` (kind/file filters), `search_commits`, `file_history`, `search_all` |
| **Semantic Search** | `oh_search_context` — natural language search over `.oh/` artifacts + git commits |
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
└── metis/           <- learnings that compound (the institutional memory)
```

Outcomes declare `files:` patterns linking to code. Commits tag `[outcome:X]` linking to outcomes. These structural links power `outcome_progress`.

`.oh/` is a **cache**, not source of truth. Outcomes originate in external systems (OH graph, Jira, Linear). `.oh/` is the repo-local, git-versioned projection. `rm -rf .oh/` loses context but breaks nothing.

## How Semantic Search Works

`oh_search_context` uses local embeddings to let agents find relevant `.oh/` artifacts and git commits by describing what they need, rather than listing and filtering. Filter by type: `outcome`, `signal`, `guardrail`, `metis`, or `commit`.

**How it works:**

1. On first search call, `.oh/` artifacts (outcomes, signals, guardrails, metis) are chunked by heading and embedded using BAAI/bge-small-en-v1.5 via fastembed-rs.
2. The agent describes what it needs in natural language — e.g., "guardrails about backward compatibility" or "learnings from previous auth refactors".
3. LanceDB performs vector similarity search and returns ranked results.
4. Results include the artifact type, file path, matched section, and similarity score.

**Why this matters:** Without semantic search, agents must call `oh_get_guardrails` and read every artifact to find the relevant ones. With `oh_search_context`, they describe the intent and get back only what matches. This replaces "list all, then filter" with "describe what you need."

**Filtering by type:** Pass `types: ["guardrail", "metis"]` to restrict search to specific artifact categories. Omit to search everything.

**Locality:** Everything runs locally. The embedding model (ONNX, ~33MB) downloads once and runs on CPU via fastembed-rs. No API calls, no cloud dependency.

## Design Decisions

- **Structural joins > semantic search for the core path** — `outcome_progress` follows links, not keywords. Semantic search via `oh_search_context` complements this for the discovery path: finding relevant context when you don't know the exact artifact name.
- **Write tools close the feedback loop** — without `oh_record_metis` and `oh_record_signal`, the system is read-only and can't compound.
- **Honest tool names** — `search_all` is multi-source grep. `outcome_progress` is the real join. `oh_search_context` is vector similarity over `.oh/`.
- **Alignment is the constraint** — not a hypothesis to measure. Session 1 exercised the full read-write loop on real work. The system compounds by design.
- **Skills integrate via context, not code** — agents read preamble sections telling them which MCP tools to call. No fork needed.

## Architecture

```
fastembed-rs      <- local ONNX embeddings (BAAI/bge-small-en-v1.5)
LanceDB           <- vector store for semantic search over .oh/ artifacts
tree-sitter       <- Rust code -> symbols (functions, structs, traits, impls)
pulldown-cmark    <- markdown -> heading-delimited chunks + code spans
git2              <- commit history, file changes, outcome tagging
rust-mcp-sdk      <- MCP server (stdio default, HTTP optional)
```

No cloud dependency. Everything is local, git-versioned, and disposable. The embedding model downloads once on first use and runs on CPU.

## Status

**Validated on 3 repos** — repo-native-alignment (self-referential), fspulse (Rust CLI), innovation-connector (enterprise Python+TS monorepo). Cold-started on all three, returned real context.

- 17 MCP tools, 20 tests, stdio + HTTP transport
- Semantic search via fastembed-rs + LanceDB (local ONNX, no API key)
- Phase agents installed, skills integrated
- Full read-write feedback loop exercised on real work
- 7 metis entries, 6 guardrails, 2 signals — all recorded via MCP tools

**Known gaps:**
- Broad `files:` patterns make `outcome_progress` noisy — need sharper globs or commit tagging
- `[outcome:X]` commit tagging convention not yet adopted on any repo besides this one
- `oh_init` scaffolds templates — should pull from OH graph for grounded context (P6)
- `setup` command configures RNA MCP only — OH MCP server install requires a separate step
