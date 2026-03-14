---
id: dogfood-rna-tools
outcome: context-assembly
severity: hard
statement: This project IS the RNA MCP server. Use its own tools for code exploration, not Grep/Read/Bash. Every fallback is a friction event that must be logged.
---

## Rationale

If the tools aren't good enough for the project that builds them, they aren't good enough for anyone. Dogfooding surfaces friction that tests and benchmarks miss — real usage in real workflows.

## What this means

- Use RNA MCP tools as the primary interface for code navigation, artifact search, and context gathering
- When RNA tools fall short, **log the friction** in the session's friction log table with severity
- A session with 0 logged friction events and many Grep/Read calls is unmonitored, not frictionless

## Override Protocol

Grep/Read are acceptable for targeted file edits (after RNA has oriented you), files not in the scan path, or when the index is genuinely down (say so explicitly).

## Evidence

Multiple sessions where agents defaulted to Grep/Read despite RNA tools being available. The friction was invisible until explicitly tracked.
