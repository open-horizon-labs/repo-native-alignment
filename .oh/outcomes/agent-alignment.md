---
id: agent-alignment
status: active
mechanism: "Agents read structured outcome/signal/constraint artifacts from repo at session start"
files:
  - "src/server.rs"
  - "src/oh/*"
  - "src/query.rs"
  - "src/types.rs"
  - ".oh/outcomes/*"
  - ".oh/signals/*"
  - ".oh/guardrails/*"
---

# Agent Alignment to Business Outcomes

Agents working in this codebase stay aligned to declared business outcomes because those outcomes are queryable artifacts, not scattered prose.

## Signals
- agent-scoping-accuracy (see signals/)

## Constraints
- repo-native (see guardrails/)
- lightweight (see guardrails/)
