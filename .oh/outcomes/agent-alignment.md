---
files:
- src/server.rs
- src/oh/*
- src/query.rs
- src/types.rs
- src/code/*
- src/git/*
- src/markdown/*
- src/walk.rs
- src/main.rs
- src/lib.rs
- .oh/outcomes/*
- .oh/signals/*
- .oh/guardrails/*
- .oh/metis/*
id: agent-alignment
mechanism: 'Agents read structured outcome/signal/constraint artifacts from repo at session start via 16 MCP tools. outcome_progress joins layers structurally. OH Skills guide the workflow. The feedback loop compounds: work → record metis → next session reads metis → agent scopes better. Validated by use: session 1 exercised the full read-write loop on real work.'
status: active
---

# Agent Alignment to Business Outcomes

Agents working in this codebase stay aligned to declared business outcomes because those outcomes are queryable artifacts, not scattered prose.

## Signals
- agent-scoping-accuracy (see signals/)

## Constraints
- repo-native (see guardrails/)
- lightweight (see guardrails/)


