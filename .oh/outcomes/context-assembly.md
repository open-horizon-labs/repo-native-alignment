---
id: context-assembly
status: active
mechanism: |-
  Agents get the fractal, local knowledge they need for a given task without manual context
  loading. Foundational models compress common human knowledge; RNA supplements with the
  context specific to a given codebase and problem.

  Mechanism: incremental scanning, pluggable multi-language extraction, unified code graph,
  semantic search, structural joins, and auto-injection of business context.

  Quality over quantity — better ranking, richer graph edges, and complete extraction matter
  more than new features.
files:
- src/extract/*
- src/scanner.rs
- src/embed.rs
- src/graph/*
- src/roots.rs
- src/main.rs
- src/lib.rs
- src/git/*
---

# Context Assembly

Agents get the right context for their task on the first or second search — code symbols,
business artifacts, structural relationships — without keyword-tuning or fallback to grep.

## Why This Matters

Foundational models compress common human knowledge. But every codebase has fractal, local
knowledge that isn't in the training data: architecture decisions, naming conventions,
business intent, dependency topology. RNA assembles this context so agents don't have to
stumble into it through dozens of grep/read cycles.

## Signals
- Fewer tool calls to reach actionable context in a typical task session
- Natural language queries returning relevant results in top-3

## Constraints
- repo-native (see guardrails/)
- extractors-are-pluggable (see guardrails/)
- extract-fully-at-parse-time (see guardrails/)
