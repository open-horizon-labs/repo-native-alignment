---
id: context-assembly
status: active
mechanism: |-
  RNA is a local context discovery and alignment tool. It makes the fractal,
  local knowledge in a codebase — the stuff not in training data — discoverable
  and queryable by coding agents.

  If an agent can't find a function, trace its callers, or understand a
  relationship through RNA, RNA is broken. Not the agent. Not the query.
  RNA.

  Mechanism: incremental scanning, pluggable multi-language extraction, unified
  code graph, semantic search, structural joins, and auto-injection of business
  context.
files:
- src/extract/*
- src/scanner.rs
- src/embed.rs
- src/graph/*
- src/roots.rs
- src/main.rs
- src/lib.rs
- src/git/*
- src/service.rs
- src/server/*
---

# Context Assembly

RNA is a local context discovery and alignment tool for coding agents.

Agents get the right context for their task on the first or second search — code symbols,
business artifacts, structural relationships — without keyword-tuning or fallback to grep.

If something exists in the codebase and RNA can't find it, that's a bug in RNA.

## Why This Matters

Foundational models compress common human knowledge. But every codebase has fractal, local
knowledge that isn't in the training data: architecture decisions, naming conventions,
business intent, dependency topology. RNA discovers and surfaces this context so agents
don't have to stumble into it through dozens of grep/read cycles.

## Signals
- Fewer tool calls to reach actionable context in a typical task session
- Natural language queries returning relevant results in top-3
- Zero classes of code symbol that RNA can't find (functions, structs, private, public — all of it)

## Constraints
- repo-native (see guardrails/)
- extractors-are-pluggable (see guardrails/)
- extract-fully-at-parse-time (see guardrails/)
- dogfood-rna-tools (see guardrails/)
