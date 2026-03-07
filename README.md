# Repo-Native Alignment

Agents stay aligned to declared business outcomes — not just code correctness — because outcomes, constraints, signals, and learnings live *in the repo* as queryable artifacts.

## The Stack

```
OH Skills (/aim, /salvage, /review, ...)    ← workflow: frame, build, reflect
    │
    ▼ calls
RNA MCP Server (16 tools)                    ← structured read/write of .oh/ + code + git
    │
    ▼ reads/writes
.oh/ directory                               ← outcomes, signals, guardrails, metis
    │
    ▼ versioned by
git                                          ← history of everything, including intent
```

**The loop:** Skills guide the workflow. MCP tools read and write structured context. `.oh/` accumulates learnings. Git versions everything. Next session starts richer than the last.

## Quick Start

```bash
# 1. Build the MCP server
cargo build --release

# 2. Configure Claude Code (add to .mcp.json)
{
  "mcpServers": {
    "rna-server": {
      "type": "stdio",
      "command": "./target/release/repo-native-alignment",
      "args": ["--repo", "."]
    }
  }
}

# 3. Install OH Skills (optional but recommended)
npx skills add open-horizon-labs/skills -g -a claude-code -y

# 4. Scaffold .oh/ for your project
# In Claude Code, call the oh_init tool — it reads your project and creates
# outcome, signal, and guardrail templates.
```

## MCP Tools (16)

### Business Context (read)
| Tool | Description |
|------|-------------|
| `oh_get_outcomes` | List outcomes from `.oh/outcomes/` |
| `oh_get_signals` | List SLO signals from `.oh/signals/` |
| `oh_get_guardrails` | List guardrails from `.oh/guardrails/` |
| `oh_get_metis` | List learnings from `.oh/metis/` |
| `oh_get_context` | Full context bundle (capped at 50 symbols/chunks) |

### Business Context (write)
| Tool | Description |
|------|-------------|
| `oh_record_metis` | Record a learning or decision |
| `oh_record_signal` | Record a signal observation (SLO measurement) |
| `oh_update_outcome` | Update outcome status/mechanism/files |
| `oh_record_guardrail_candidate` | Propose a guardrail from experience |
| `oh_init` | Scaffold `.oh/` from existing project context |

### Search
| Tool | Description |
|------|-------------|
| `search_markdown` | Search all `.md` files by content/headings |
| `search_code` | Search code symbols with optional `kind` and `file` filters |
| `search_commits` | Search git commit messages |
| `file_history` | Git history for a specific file |
| `search_all` | Multi-source substring search (honest name: it's grep across layers) |

### Intersection Query
| Tool | Description |
|------|-------------|
| `outcome_progress` | **The real join:** outcome → tagged commits + file pattern matches → code symbols → related markdown |

## How `outcome_progress` Works

This is the tool that connects layers structurally, not by keyword:

1. Finds the outcome by ID from `.oh/outcomes/`
2. Finds commits tagged `[outcome:{id}]` in their message
3. Finds commits touching files matching the outcome's `files:` patterns
4. Deduplicates, finds code symbols in changed files
5. Finds markdown sections mentioning the outcome
6. Returns a connected answer: outcome → commits → code → docs

## The `.oh/` Directory

```
.oh/
├── outcomes/           ← what we're optimizing for
│   └── agent-alignment.md
├── signals/            ← how we measure progress
│   └── agent-scoping-accuracy.md
├── guardrails/         ← constraints that shape behavior
│   ├── repo-native.md
│   └── lightweight.md
└── metis/              ← learnings that compound
    ├── session-1-salvage.md
    └── commit-tagging-convention.md
```

Each file is structured markdown with YAML frontmatter. Outcomes can declare `files:` patterns to link to code. Commits can tag `[outcome:X]` to link to outcomes. These links power `outcome_progress`.

## With OH Skills

When [OH Skills](https://github.com/open-horizon-labs/skills) are installed, each skill in the workflow knows how to use RNA MCP tools:

| Skill | RNA MCP Integration |
|-------|-------------------|
| `/aim` | Reads existing outcomes before framing, updates outcome after |
| `/problem-space` | Loads full context, checks outcome progress |
| `/solution-space` | Validates against guardrails, records decision rationale as metis |
| `/execute` | Pre-flight guardrail check, tags commits with `[outcome:X]` |
| `/ship` | Records signal observations, updates outcome status |
| `/review` | Checks work against declared outcome, surfaces missing guardrails |
| `/dissent` | Grounds dissent in declared constraints, records findings as metis |
| `/salvage` | Records learnings as metis, captures guardrail candidates |

The skills guide the workflow. The MCP tools make it persistent.

## Architecture

```
tree-sitter       ← parse Rust code → symbols (functions, structs, traits)
pulldown-cmark    ← parse all markdown → heading-delimited chunks + code spans
git2              ← commit history, file changes, blame
rust-mcp-sdk      ← MCP server (stdio + HTTP transport)
```

No external database. No cloud dependency. Everything is local, git-versioned, and disposable — `rm -rf .oh/` loses context but breaks nothing.

## Design Decisions

- **`.oh/` is a cache, not source of truth** — outcomes originate in external systems (OH graph, Jira, Linear, Notion). `.oh/` is the repo-local projection.
- **Structural joins over semantic search** — `outcome_progress` follows links (file patterns, commit tags), not keyword matches. Embeddings are future work for the discovery path.
- **Honest tool names** — `search_all` is multi-source grep, not an intersection query. `outcome_progress` is the real join.
- **Write tools close the feedback loop** — without `oh_record_metis` and `oh_record_signal`, the system is read-only and can't compound.

## Status

Working prototype. 16 MCP tools, 20 tests, stdio + HTTP transport. See `.oh/repo-native-alignment.md` for the full session history.
