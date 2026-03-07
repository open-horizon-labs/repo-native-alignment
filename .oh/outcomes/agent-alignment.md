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
- src/roots.rs
- .oh/outcomes/*
- .oh/signals/*
- .oh/guardrails/*
- .oh/metis/*
id: agent-alignment
mechanism: |-
  Two converging aims under one outcome:

  **Agent alignment:** Agents stay on-task with declared business outcomes via 9 intent-based MCP tools (consolidated from 20+), context injection on first tool call, outcome_progress structural joins, and OH Skills integration.

  **Workspace context engine:** Agents see richer context via incremental scanner (mtime + git), 8 pluggable extractors (tree-sitter: Rust/Python/TS/Go + pulldown-cmark: Markdown + line-based: Proto/SQL/OpenAPI), unified graph (LanceDB + petgraph), multi-language LSP enrichment (rust-analyzer/pyright/tsserver/gopls/marksman), PR merge history, and multi-root workspace scanning.

  The feedback loop: skills guide work → RNA produces evidence → OH stores it → reflect distills it → next session starts richer.
status: active
---

# Agent Alignment to Business Outcomes

Agents working in this codebase stay aligned to declared business outcomes because those outcomes are queryable artifacts, not scattered prose.

## Signals
- agent-scoping-accuracy (see signals/)

## Constraints
- repo-native (see guardrails/)
- lightweight (see guardrails/)





