# Session: repo-native-alignment

## Aim
**Updated:** 2026-03-06

Agents working in a codebase stay aligned to declared business outcomes — not just code correctness — because the outcomes, constraints, and progress signals live *in the repo* as queryable, evolving artifacts that agents read from and write to during normal work.

**Current → Desired:** Agents operate in a vacuum, rediscovering intent every session from scattered prose. → An agent starting a session can query "what are the active outcomes? what constraints apply? what have we learned?" and get structured answers from the repo itself.

**Guardrails:**
- **Repo-native** — no external store, no platform dependency. `rm -rf .oh/` loses context but breaks nothing.
- **Lightweight** — adding an outcome is writing a markdown file, not configuring a system.
- **Git-versioned** — every change is a commit. History is free.
- **Agent-writable, human-curated** — agents propose metis; humans promote to guardrails.

---

## Problem Statement
**Updated:** 2026-03-06

Agents can't answer "what have we done towards this outcome?" because there's no system connecting business context (aims, signals, guardrails, metis), code structure, and git history into a single queryable interface. The three exist in isolation — `.oh/` files, source code, git log — with no way to intersect them.

**The shift:** From "prove the value of structured context" → "build the system that makes business context, code, and history queryable as one thing."

### Core Architecture Decisions (settled)

**Markdown is input AND output.** All markdown (CLAUDE.md, README, ADRs, `.oh/` files) is semantic context — first-class parsing with pulldown-cmark and embedding, not just `.oh/`. MCP responses are markdown-native: tables, `file:line` links, diff blocks.

**Hybrid index:** Lightweight persistent index (symbol names, file locations, heading hierarchy, change timestamps) + on-demand deep reads (full AST, embeddings, file contents). Without the index, "just-in-time context" means "grep and hope."

**Extractor pipeline:** Pluggable extractors produce `{ metadata, chunks }`. MarkdownExtractor (pulldown-cmark): heading-delimited sections, code span cross-references, YAML frontmatter. TreeSitterExtractor: functions/structs/traits/imports with metadata. git2 drives delta: only changed files re-extracted.

**`.oh/` is a cache, not source of truth.** Outcomes originate in OH graph, Jira, Notion, etc. `.oh/` is the repo-local projection you can `git commit`. The vector store is the real index.

**Stack:** git2 (change detection) → tree-sitter (code) + pulldown-cmark (markdown) → LanceDB (columnar + vectors + full-text) → MCP server. DuckDB optional analytics overlay.

### Constraints
- **Hard:** repo-native, lightweight, MCP interface, must connect all three layers (aims + code + history), all markdown first-class, `.oh/` is a cache
- **Soft:** Rust + rust-mcp-sdk, YAML frontmatter, LanceDB, pulldown-cmark + tree-sitter

---

## System Phases

| Phase | Description | Status |
|---|---|---|
| 0: Bootstrap | Read `.oh/` from disk, 6 MCP tools, no indexing | ✅ Shipped |
| 1: Markdown + Embeddings | pulldown-cmark all `.md`, LanceDB, semantic search | ✅ Shipped |
| 2: Code Scanner | tree-sitter symbols, embeddings on bodies | ✅ Shipped |
| 3: Git Awareness | git2, incremental indexing, commit↔symbol mapping | ✅ Shipped |
| 4: Query Engine | Hybrid queries, natural language → multi-layer answer | 🔄 Current |
| 5: OH Sync | Bidirectional `.oh/` ↔ OH graph, git push/pull semantics | ⬜ Future |

**Current state (2026-03-08):** Background scanner (15min), persisted embeddings, incremental graph updates (RwLock + scan cooldown), commit hash surfaced in `oh_search_context`. `git_history` tool dropped — commit info available via search results directly.

---

## The Query That Proves It Works

> "What have we done to make alignment available via MCP?"

Exercises the full stack: outcome lookup → metis search → git history → code search (MCP tool definitions) → markdown search (session sections) → synthesized timeline. When this query returns a grounded, multi-layer answer, the system works.

---

## Repo Structure

```
my-project/
├── .oh/
│   ├── outcomes/        ← aim + mechanism + signals
│   ├── signals/         ← SLO definitions + thresholds
│   ├── guardrails/      ← hard constraints with severity
│   ├── metis/           ← "we tried X, learned Y" (contextual, human-curated)
│   └── sessions/        ← active work context
├── CLAUDE.md            ← code conventions
├── src/                 ← the code
└── .git/                ← history of everything above
```

---

## Connections

**CodeState convergence:** Same architecture viewed from two angles — code structure queryable (CodeState) vs. business intent queryable (this). Same extractors, same pipeline, different content. The harness IS the CodeState vision applied to the full agent working context.

**Open Horizons:** `.oh/` is the local projection of the OH graph into a repo. `.oh/outcomes/` ↔ OH Aims, `.oh/signals/` ↔ OH Signals, `.oh/guardrails/` ↔ OH Guardrails, `.oh/metis/` ↔ OH Metis. Sync is Phase 5.

**Artium/Ross:** outcomes + signals make reliability measurable per project; guardrails as repo artifacts with severity; `.oh/` IS the "seed that grows per client"; earned trust dial maps to guardrail severity relaxing as metis accumulates evidence.

---

## New Aim: Human-Led Curation (2026-03-08)

Two MCP tools that assist human judgment without replacing it — directly from the principle that LLMs can extract themes but cannot judge what matters.

**`oh_propose_themes`** — surfaces patterns across accumulated metis. Clusters by semantic similarity, proposes candidates for compaction or guardrail promotion. Human reviews, selects, discards. Addresses the problem: after 20+ sessions, no human can hold all metis in mind to notice what's recurring.

**`oh_propose_relevant`** — given current task, phase, and active outcome, ranks and filters the metis+guardrails corpus by likely relevance. Returns a short candidate list with reasoning. Human selects what to load into context.

Neither tool makes decisions. Both reduce cognitive load in the *search-and-surface* step — which is where LLMs have leverage without needing judgment. The judgment (what to keep, promote, apply) stays human.

**Guardrails for this aim:**
- Proposals are candidates, never decisions
- Auto-promotion to guardrail never happens without a human writing it
- Phase-awareness is mandatory — `oh_propose_relevant` must weight phase tags heavily; cross-phase metis pollution is a known failure mode
- Each proposal links to source metis IDs (provenance is non-negotiable)

See outcome: `.oh/outcomes/human-led-curation.md`

---

## External Exploration: always-on-memory-agent (2026-03-08)

Assessed `GoogleCloudPlatform/generative-ai/.../always-on-memory-agent`. Net verdict: nothing worth porting. Instructive as a contrast case.

**What it does:** Python daemon — watches `./inbox/`, LLM-extracts memories to SQLite, auto-consolidates every 30min via LLM to find patterns, serves via HTTP.

**Why nothing is worth porting:**
- Multimodal inbox ingestion — wrong domain (we index git repos, not dropped files)
- Flat SQLite memory store — LanceDB is strictly better
- "No embeddings" — we use vectors intentionally
- HTTP API — MCP is our protocol
- **Auto-consolidation via LLM** — see metis below

**Two principles reinforced by studying this system:**

1. **LLMs are not for judgment** (`llm-synthesis-is-not-judgment.md`). LLMs can extract themes; they cannot decide what's significant, what should govern future behavior, what warrants promotion to a guardrail. Auto-consolidating memories creates authoritative-looking "insights" that reflect no situated experience. The human-curation step in metis→guardrail is the point, not an inefficiency.

2. **Metis is contextual, not universal** (`metis-is-not-universal.md`). What works in one phase/context/task type does not carry universally. Selecting appropriate metis for a given situation is itself a cognitive act — not a lookup. The harness enables selection; it does not replace it. Building a system that auto-applies accumulated metis indiscriminately produces worse decisions than no system at all.
