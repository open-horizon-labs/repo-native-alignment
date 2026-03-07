# Session: P3 — Skills Bundle + MCP Synergy

## Aim
**Updated:** 2026-03-07

Package the OH skills (aim, problem-space, problem-statement, solution-space, execute, review, dissent, salvage, ship, teach-oh) together with the RNA MCP server so they work synergistically. If the user follows the workflow, the skills read/write `.oh/` via the MCP tools, and the tools get richer because the skills populate them.

The skills already reference `.oh/` session files. The MCP tools already read/write `.oh/`. The gap: they don't know about each other. A skill running `/salvage` should call `oh_record_metis`. A skill running `/aim` should call `oh_get_outcomes` to see what exists. The integration is the product.

## Problem Statement
**Updated:** 2026-03-07

The skills (open-horizon-labs/skills) and the MCP server (this repo) are separate. The skills write `.oh/` session files via file I/O. The MCP tools read/write `.oh/` artifacts via structured APIs. They operate on the same directory but don't talk to each other.

**The opportunity:** If skills used MCP tools instead of raw file I/O, then:
1. Skills get structured read/write (not string manipulation of markdown)
2. MCP tools get exercised naturally through the workflow
3. `.oh/` stays consistent (one writer, not two)
4. The feedback loop compounds: /salvage → oh_record_metis → next session reads metis → agent scopes better

## Findings from Skills Repo

The skills repo has:
- 10 SKILL.md files (prompt-only skills for Claude Code)
- `agents-omp/` — agent wrappers for oh-my-pi
- `hooks-omp/` — phase-aware hook that suggests next skill
- `teach-oh/` — project setup that writes AGENTS.md
- `.agents/skills/` — duplicated skill files in agents format

Key insight from README: skills work at 3 levels:
1. **Base** — just the prompt, no deps
2. **With .oh/ session** — reads/writes session files
3. **With OH MCP** — full graph integration

Level 2 is what we're enhancing. The skills already know about `.oh/`. They just don't use the MCP tools to interact with it.

## Options

### Option A: Fork skills into this repo
- Copy SKILL.md files into this repo
- Modify them to reference RNA MCP tools
- Ship as one installable package
- Trade-off: fork divergence from upstream skills repo

### Option B: Keep skills separate, add MCP awareness
- Skills repo stays independent
- Add "With RNA MCP" adaptive section to each skill
- Skills detect if rna-server tools are available and use them
- Trade-off: coordination across repos

### Option C: Skills as a plugin/extension of this repo
- This repo ships the MCP server + a skills directory
- `npx skills add open-horizon-labs/repo-native-alignment -g` installs both
- Skills and MCP tools version-locked
- Trade-off: this repo does two things (server + skills)

### Option D: Integration via context, not code
- Skills don't change at all
- `oh_init` writes a CLAUDE.md section mapping skills → MCP tools
- Agent reads CLAUDE.md and follows the integration instructions
- Trade-off: depends on agent reading instructions (which it already does)

---

## New Finding: skills PR #8

PR #8 (`teach-oh-phase-agents`) already has the adaptive MCP integration pattern:
- Lines 361-373: checks if OH MCP tools are available, appends an MCP preamble to agent files
- Detection: looks for `oh_get_endeavors` in available tools, or `.oh/mcp.json`
- Pattern: conditional append, not fork — exactly the approach we need

This means Option B is already half-built in upstream. We don't need Option D (CLAUDE.md hack). We need to extend PR #8's pattern to detect RNA MCP tools alongside OH MCP tools.

## Solution Space (Revised)
**Updated:** 2026-03-07

**Selected:** Option B — extend PR #8's MCP preamble pattern for RNA tools
**Level:** Local Optimum (the pattern already exists, we're extending it)

**How it works:**
1. PR #8 already appends an "Open Horizons MCP" preamble to agent files when OH MCP is detected
2. Add a parallel "RNA MCP" preamble that gets appended when rna-server tools are detected
3. Detection: check for `oh_get_outcomes` (rna-server tool) or `.mcp.json` with rna-server
4. The RNA preamble tells agents: "use oh_get_outcomes before framing, use oh_record_metis after salvage, use outcome_progress to check alignment"

**The RNA MCP preamble for each agent:**

```markdown
## Repo-Native Alignment MCP
When rna-server MCP tools are available:
- Before framing: call `oh_get_outcomes` and `oh_get_guardrails` to load business context
- After producing output: call `oh_record_metis` to capture key learnings
- When checking progress: call `outcome_progress` with the relevant outcome ID
- When discovering constraints: call `oh_record_guardrail_candidate`
- When measuring progress: call `oh_record_signal`
```

**Per-agent specifics:**
- `oh-aim`: call `oh_get_outcomes` first, call `oh_update_outcome` after
- `oh-execute`: call `outcome_progress` for context, tag commits with `[outcome:X]`
- `oh-solution-space`: call `oh_get_guardrails` for constraints
- `oh-problem-space`: call `oh_get_context` for full picture

**Why this beats Option D:**
- Integrates at the agent/skill level, not CLAUDE.md
- Uses the same pattern PR #8 already established
- Skills repo owns the integration, no fork needed
- Works for both skill and agent variants

**Accepted trade-offs:**
- Requires a PR to the skills repo (but it extends an existing PR, not a new pattern)
- RNA MCP detection is heuristic (tool name check)

### Implementation Checklist
- [ ] PR to skills repo: add RNA MCP preamble alongside OH MCP preamble in teach-oh Step 5
- [ ] Define the per-agent RNA MCP instructions
- [ ] Update detection logic: check for `oh_get_outcomes` (RNA) alongside `oh_get_endeavors` (OH)
- [ ] Test: install skills + RNA MCP on a project, run /teach-oh, verify preamble appended
- [ ] Optionally: `oh_init` in this repo can also write the integration instructions to CLAUDE.md as a fallback for non-OMP users
