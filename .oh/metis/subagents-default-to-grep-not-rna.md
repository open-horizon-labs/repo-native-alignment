---
id: subagents-default-to-grep-not-rna
outcome: agent-alignment
title: Spawned sub-agents default to grep/Read — RNA and LSP must be explicitly required
---

## What Happened

Six sub-agents were spawned in parallel to fix issues #33–#37 and review PR #31. Tool usage audit showed all four running agents used only Bash/Read/Edit/Grep — none called `oh_search_context`, `search_symbols`, or LSP tools.

## Root Cause

Agent prompts described *what* to implement but didn't explicitly require RNA MCP or LSP tools for code exploration. Agents defaulted to the lowest-friction path: grep/Read.

## The Fix

Sub-agent prompts must explicitly instruct:
1. Use `oh_search_context` before reading files to surface relevant metis and guardrails
2. Use `search_symbols` for code exploration instead of Grep/Glob
3. Use `LSP` tool for definition/type lookups instead of grepping for type names

## Template Addition for Agent Prompts

Add this block to every sub-agent prompt that does code exploration:

```
## Tool usage requirements
- Use `oh_search_context` (RNA MCP) BEFORE reading files — surface relevant metis and guardrails first
- Use `search_symbols` (RNA MCP) for code navigation instead of Grep/Glob
- Use LSP tools for type/definition lookups instead of text search
- Grep/Read are last resort, not first instinct
```

## Why It Matters

Sub-agents that only grep miss the accumulated learnings in `.oh/metis/` and guardrails that would change their implementation approach. The compounding value of RNA requires agents to actually read it.

