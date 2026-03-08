---
id: rna-directive-quantifiably-changes-agent-behavior
outcome: agent-alignment
title: 'RNA directive quantifiably changes agent behavior: 1-7 calls without vs 27+ with'
---

## What Happened

In this session's batch, sub-agents were spawned to fix LSP issues (#33–#37). Two groups:
- **Without explicit RNA directive:** agents used 1-7 RNA/LSP calls, defaulted to grep/Read for most exploration
- **With explicit RNA directive** (the "Tool usage requirements" block): agents used 27+ RNA calls — oh_search_context before file reads, search_symbols for navigation, LSP for type lookups

## Why It Matters

This is the first quantitative evidence that the sub-agent RNA directive works as intended. The directive isn't just documentation — it changes the tool selection path. Without it, agents fall through to lowest-friction tools (grep). With it, they use the accumulated context layer.

## Implication

The sub-agent prompt template (from `subagents-default-to-grep-not-rna.md`) is validated. It must be included in every agent prompt that does code exploration. The 4x+ increase in RNA usage translates directly to agents surfacing relevant metis and guardrails that would otherwise be missed.

## Evidence Source

PRs #33–#44 batch, Mar 8 session. Audited tool call logs from parallel sub-agents.
