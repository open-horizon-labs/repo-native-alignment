# Open Horizons Framework

**The shift:** Action is cheap. Knowing what to do is scarce.

**The sequence:** aim → problem-space → problem-statement → solution-space → execute → ship

**Where to start (triggers):**
- Can't explain why you're building this → `/aim`
- Keep hitting the same blockers → `/problem-space`
- Solutions feel forced → `/problem-statement`
- About to start coding → `/solution-space`
- Work is drifting or reversing → `/salvage`

**Reflection skills (use anytime):**
- `/review` - Check alignment before committing
- `/dissent` - Seek contrary evidence before one-way doors
- `/salvage` - Extract learning, restart clean

**Key insight:** Enter at the altitude you need. Climb back up when you drift.

## Repo-Native Alignment MCP

This project IS the RNA MCP server. When working here, use its own tools:

- Before starting work: call `oh_get_outcomes` and `oh_get_guardrails`
- After completing work: call `oh_record_metis` with key learnings
- When checking progress: call `outcome_progress` with `agent-alignment`
- When discovering constraints: call `oh_record_guardrail_candidate`
- When measuring progress: call `oh_record_signal`
- Tag commits with `[outcome:agent-alignment]`

---

# Project Context

## Purpose
MCP server that makes business outcomes, code structure, markdown, and git history queryable as one system. Agents stay aligned to declared intent because that intent lives in the repo as structured, queryable artifacts.

## Current Aims
- **agent-alignment** (active): Agents scope work to declared outcomes without user re-prompting. Mechanism: 16 MCP tools + OH Skills integration + outcome_progress structural joins.

## Key Constraints
- **repo-native** (hard): No external store. `.oh/` in the repo, git-versioned. `rm -rf .oh/` loses context but breaks nothing.
- **lightweight** (hard): Adding an outcome = writing a markdown file. If heavier than a CLAUDE.md section, adoption fails.
- **name-tools-honestly** (soft): Tool names describe current behavior, not aspirations.
- **test-with-real-mcp-client** (candidate): Test MCP changes with TypeScript SDK or Claude Code, not curl.
- **validate-before-building** (soft): Don't add infrastructure before validating the hypothesis. Behavior change is the metric.

## Patterns to Follow
- `[outcome:X]` in commit messages to link work to outcomes
- Use MCP write tools (oh_record_metis, oh_record_signal, oh_record_guardrail_candidate) to close the feedback loop
- Structural joins (outcome_progress) over keyword search (search_all) for the core use case
- BTreeMap for frontmatter (deterministic output)
- YAML frontmatter + markdown body for all `.oh/` artifacts
- `cargo build --release` before `/mcp` reconnect

## Anti-Patterns to Avoid
- Don't search function bodies in code search (noise) — match name + signature only
- Don't add embeddings/LSP/multi-language before validating agent behavior change
- Don't call a union of four greps an "intersection query"
- Don't test MCP with curl alone — protocol negotiation differs from real clients

## Decision Context
Solo developer. PRs get /review and /dissent before merge. "Done" = tests pass, MCP client connects, tools exercised through real usage. Session learnings recorded as metis via MCP tools.

## Unvalidated Hypotheses
- agent-scoping-accuracy SLO has zero observations
- Compound metis effect untested (3 entries, needs 5-10 sessions)
- Not tested on a second non-trivial repo
