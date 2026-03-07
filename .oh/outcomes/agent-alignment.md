---
files:
- src/server.rs
- src/graph/*
- src/extract/*
- src/scanner.rs
- src/embed.rs
- src/query.rs
- src/main.rs
- src/lib.rs
- src/git/*
- .oh/outcomes/*
- .oh/signals/*
- .oh/guardrails/*
- .oh/metis/*
id: agent-alignment
mechanism: |-
  Two converging aims under one outcome:

  **Agent alignment:** Agents stay on-task with declared business outcomes via 20 MCP tools (consolidating to ~8 intent-based tools per #25), context injection on first tool call (#22), outcome_progress structural joins, and OH Skills integration.

  **Workspace context engine:** Agents see richer context via incremental scanner (mtime + git), pluggable extractors (tree-sitter: Rust/Python/TS/Go/Markdown), unified graph (LanceDB + petgraph), LSP enrichment (#9 in progress), schema extraction (#10 in progress), PR merge history (#11 in progress), and multi-root workspace scanning (#12 in progress).

  The feedback loop: skills guide work → RNA produces evidence → OH stores it → reflect distills it → next session starts richer.

  ## Status of Capabilities

  ### Shipped
  - 20 MCP tools (search_symbols, graph_neighbors, graph_impact, oh_search_context, outcome_progress, etc.)
  - Incremental scanner with mtime skip + git optimization + configurable excludes (.oh/config.toml)
  - 5 pluggable extractors: Rust, Python, TypeScript, Go, Markdown (tree-sitter + pulldown-cmark)
  - Unified graph model: LanceDB schemas + petgraph in-memory traversal
  - PR merge graph types (NodeKind::PrMerge, EdgeKind::Modified/Affected/Serves)
  - Source-capability layer (SourceEnvelope, Scope, provenance on all nodes/edges)
  - Context injection on first MCP tool call (#22)
  - Semantic search over .oh/ artifacts (fastembed + LanceDB)
  - Full read-write feedback loop (record metis, signals, guardrails via MCP)

  ### In Progress
  - #9: LSP enricher — Calls/Implements edges via rust-analyzer, pyright, tsserver, gopls
  - #10: Schema extractors — .proto, SQL migrations, OpenAPI
  - #11: PR merge extraction — git merge history walker → graph nodes + edges
  - #12: Multi-root workspace — ~/.config/rna/roots.toml, per-root scanning, list_roots tool

  ### Queued
  - #25: Tool consolidation — 20+ tools → ~8 intent-based tools (critical — tool proliferation hurts agent effectiveness)
  - #23: Agents default to grep/Read instead of graph tools
  - Wire LSP/schema/PR-merge into consolidated tool interface
  - Test on non-RNA repo (Mayo, innovation-connector) with Python/TS code
  - Context injection tool recommendations ("use search_symbols, not Grep")
  - agent-scoping-accuracy SLO observation on a real project

  ### Designed but Not Started
  - Multi-root cross-root discovery (search zettelkasten from project context)
  - JIT root scanning (reference a file outside declared roots → scan on demand)
  - Runtime architecture topology (protocol, serialization, sync/async on edges)
  - Mermaid/PlantUML diagram parser → graph edges
status: active
---

# Agent Alignment to Business Outcomes

Agents working in this codebase stay aligned to declared business outcomes because those outcomes are queryable artifacts, not scattered prose.

## Signals
- agent-scoping-accuracy (see signals/)

## Constraints
- repo-native (see guardrails/)
- lightweight (see guardrails/)




