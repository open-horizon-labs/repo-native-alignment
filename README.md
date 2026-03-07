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

## Quick Start

```bash
# 1. Build the RNA MCP server
cargo build --release

# 2. Configure Claude Code (.mcp.json in project root)
cat > .mcp.json << 'EOF'
{
  "mcpServers": {
    "rna-server": {
      "type": "stdio",
      "command": "./target/release/repo-native-alignment",
      "args": ["--repo", "."]
    }
  }
}
EOF

# 3. Install OH Skills (optional, recommended)
npx skills add open-horizon-labs/skills -g -a claude-code -y

# 4. Scaffold .oh/ — run in Claude Code:
#    Call the oh_init tool, or run /teach-oh for full project setup
#    /teach-oh also installs phase agents to .claude/agents/

# 5. Start working — the system compounds from here
```

## The Four Systems

### RNA MCP Server (this repo) — 16 tools

The repo-local intelligence layer. Parses code (tree-sitter), markdown (pulldown-cmark), and git history (git2). Exposes everything via MCP.

| Category | Tools |
|----------|-------|
| **Read .oh/** | `oh_get_outcomes`, `oh_get_signals`, `oh_get_guardrails`, `oh_get_metis`, `oh_get_context` |
| **Write .oh/** | `oh_record_metis`, `oh_record_signal`, `oh_update_outcome`, `oh_record_guardrail_candidate`, `oh_init` |
| **Search** | `search_markdown`, `search_code` (kind/file filters), `search_commits`, `file_history`, `search_all` |
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
| `/review` | Check alignment before committing | Calls `outcome_progress`, `oh_get_guardrails` |
| `/dissent` | Devil's advocate before one-way doors | Grounds dissent in declared constraints |
| `/salvage` | Extract learning before restarting | Records metis and guardrail candidates |

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
├── outcomes/        ← what we're optimizing for (YAML frontmatter + markdown)
├── signals/         ← how we measure progress (SLO definitions + observations)
├── guardrails/      ← constraints that shape behavior (hard/soft/candidate)
└── metis/           ← learnings that compound (the institutional memory)
```

Outcomes declare `files:` patterns linking to code. Commits tag `[outcome:X]` linking to outcomes. These structural links power `outcome_progress`.

`.oh/` is a **cache**, not source of truth. Outcomes originate in external systems (OH graph, Jira, Linear). `.oh/` is the repo-local, git-versioned projection. `rm -rf .oh/` loses context but breaks nothing.

## Design Decisions

- **Structural joins > semantic search** — `outcome_progress` follows links, not keywords. Embeddings are future work for the discovery path.
- **Write tools close the feedback loop** — without `oh_record_metis` and `oh_record_signal`, the system is read-only and can't compound.
- **Honest tool names** — `search_all` is multi-source grep. `outcome_progress` is the real join.
- **Alignment is the constraint** — not a hypothesis to measure. Session 1 exercised the full read-write loop on real work. The system compounds by design.
- **Skills integrate via context, not code** — agents read preamble sections telling them which MCP tools to call. No fork needed.

## Architecture

```
tree-sitter       ← Rust code → symbols (functions, structs, traits, impls)
pulldown-cmark    ← all markdown → heading-delimited chunks + code spans
git2              ← commit history, file changes, outcome tagging
rust-mcp-sdk      ← MCP server (stdio default, HTTP optional)
```

No external database. No cloud dependency. Everything is local, git-versioned, and disposable.

## Status

Working prototype. 16 MCP tools, 20 tests, stdio + HTTP transport. Skills integration PR open. Phase agents installed. Full read-write loop exercised on real work.

**Next:** Grounded `oh_init` (scaffold from OH graph, not templates), then cross-references (markdown code spans → symbol table). See `.oh/p4-solution-space.md`.
