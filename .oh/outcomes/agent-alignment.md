---
id: agent-alignment
status: maintenance
mechanism: |-
  Work stays connected to declared business outcomes. Outcomes, signals, guardrails, and metis
  are queryable artifacts in the repo. Agents discover what outcomes exist, what constraints
  apply, and what progress looks like — without the user explaining it.

  Architecture is settled: intent-based MCP tools, context injection on first tool call,
  outcome_progress structural joins, commit tagging, OH Skills integration.

  Remaining work is bug fixes, tool cleanup, and adoption improvements.
files:
- src/server.rs
- src/query.rs
- .oh/outcomes/*
- .oh/signals/*
- .oh/guardrails/*
- .oh/metis/*
---

# Agent Alignment to Business Outcomes

Agents working in this codebase stay aligned to declared business outcomes because those outcomes are queryable artifacts, not scattered prose.

## Signals
- agent-scoping-accuracy (see signals/)
- tools-exercised-per-session (see signals/)

## Constraints
- repo-native (see guardrails/)
- lightweight (see guardrails/)
