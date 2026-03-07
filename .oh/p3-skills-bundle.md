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
