---
id: skills-integration-pivot
outcome: agent-alignment
title: 'Skills Integration: Context Beats Code Fork'
---

## What Happened

Initially chose Option D (write CLAUDE.md integration instructions) for connecting OH Skills to RNA MCP. Then discovered skills PR #8 already had a conditional MCP preamble pattern (lines 361-373 of teach-oh/SKILL.md) — check for OH MCP tools, append instructions to agent files.

Pivoted to Option B: extend PR #8's existing pattern to also detect RNA MCP tools. Per-agent instructions added to all 6 agent files + 3 cross-cutting skills (salvage, review, dissent).

## The Learning

Before designing an integration mechanism, check if the target already has one. PR #8's preamble pattern was exactly what we needed — extending it was a few edits, not a new architecture.

## Applied To

- skills PR #9: RNA MCP integration for agents and cross-cutting skills
- Pattern: detect `oh_get_outcomes` in available tools → append RNA preamble
