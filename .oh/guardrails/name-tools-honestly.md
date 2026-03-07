---
id: name-tools-honestly
outcome: agent-alignment
severity: soft
statement: Name MCP tools for what they actually do, not what you aspire them to do
---

The original `query` tool claimed to be an "intersection query" but was actually four independent substring matches unioned together. Agents and users make decisions based on tool descriptions. Renaming to `search_all` (honest) and building `outcome_progress` (the actual intersection) was the fix.

## Override Protocol
Acceptable to use aspirational names in session files and design docs. Tool names and descriptions seen by agents must describe current behavior.
