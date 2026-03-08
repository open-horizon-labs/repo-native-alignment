---
id: subagent-prompts-require-rna-directive
outcome: agent-alignment
severity: soft
statement: Every sub-agent prompt that performs code exploration must include an explicit "Tool usage requirements" block mandating oh_search_context before file reads, search_symbols for navigation, and LSP for type lookups. Grep/Read are last resort.
---

## Rationale

Without the explicit directive, sub-agents default to lowest-friction tools: Bash/Grep/Read. They miss accumulated metis and guardrails in `.oh/` that would change their implementation approach. The compounding value of RNA requires agents to actually read it.

## Evidence

Three independent observations:
1. Session 1 salvage: "agents fall back to grep without instructions" (session-1-salvage.md)
2. Mar 8 batch: 6 parallel sub-agents without directive used 1-7 RNA calls each (subagents-default-to-grep-not-rna.md)
3. Same batch with directive: 27+ RNA calls per agent (rna-directive-quantifiably-changes-agent-behavior.md)

The directive works. It must be included.

## Required Template Block

```
## Tool usage requirements (MANDATORY)
- Use `oh_search_context` (RNA MCP tool) BEFORE reading files
- Use `search_symbols` (RNA MCP tool) for code navigation
- Grep/Read are last resort, not first instinct
```

## Override Protocol

Read-only review agents (code review, summarization) with no exploration burden may omit this. Any agent that needs to understand what already exists in the codebase must include it.
