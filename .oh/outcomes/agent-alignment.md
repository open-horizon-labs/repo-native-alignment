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
- .oh/outcomes/*
- .oh/signals/*
- .oh/guardrails/*
- .oh/metis/*
id: agent-alignment
mechanism: 'Agents read structured outcome/signal/constraint artifacts from repo at session start via 20 MCP tools. Workspace graph (petgraph + LanceDB) provides multi-language code structure via incremental scanner + pluggable tree-sitter extractors. outcome_progress joins layers structurally. OH Skills guide the workflow. The feedback loop compounds: work → record metis → next session reads metis → agent scopes better. Validated across 3 repos and multiple sessions.'
status: active
---

# Agent Alignment to Business Outcomes

Agents working in this codebase stay aligned to declared business outcomes because those outcomes are queryable artifacts, not scattered prose.

## Signals
- agent-scoping-accuracy (see signals/)

## Constraints
- repo-native (see guardrails/)
- lightweight (see guardrails/)



