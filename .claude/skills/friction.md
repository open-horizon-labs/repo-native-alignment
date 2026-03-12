---
name: friction
description: Log RNA MCP tool friction. Use whenever an RNA tool fails, frustrates, or gets skipped in favor of Grep/Read.
---

# /friction

Log a friction event with an RNA MCP tool. This is how the product improves — every friction point is signal.

## When to Use

Call `/friction` (or apply this process inline) whenever:

- An RNA tool returned **wrong or irrelevant results**
- An RNA tool was **missing data** you expected (symbol, edge, metadata)
- The API was **awkward** — needed N calls where 1 should suffice, params confusing
- A tool was **too slow** and disrupted your flow
- You **fell back to Grep/Read** because the RNA tool wasn't sufficient
- A tool **errored or hung**

## How to Log

Write friction to `.oh/friction-logs/<pipeline-or-context>.md` — one file per pipeline run or work session. Create the file if it doesn't exist, append if it does.

**File format:**
```markdown
# Friction Log: <context>
**Date:** <date>
**Pipeline/Issue:** #<number> or <description>

| Phase/Step | Tool | What happened | Workaround | Severity |
|------------|------|---------------|------------|----------|
| <phase> | <tool name> | <what happened> | <what you did instead> | <severity> |
```

Also append to the active session file's `## RNA Tool Friction Log` table if one exists — this keeps friction visible in the session while the canonical log lives in `.oh/friction-logs/`.

**Severity levels:**
- **blocker** — had to abandon the tool entirely, couldn't complete the task with it
- **friction** — worked around it, but it cost time or context window
- **papercut** — minor annoyance, still usable

## Friction Summary

At the end of a pipeline run (or on explicit `/friction summary`), review the accumulated friction log and produce:

```markdown
## Friction Summary

**Total events:** N (X blockers, Y friction, Z papercuts)

### Patterns
- <pattern 1>: <description> (N occurrences)
- <pattern 2>: <description> (N occurrences)

### Recommendations
- **File issue:** <description> — severity warrants tracking
- **Update existing issue:** #<number> — <how this friction relates>
- **Known limitation:** <description> — not actionable yet, note for context
```

## RNA Tool Quick Reference

Use RNA tools as your primary codebase interface. Log friction when they fall short.

| Need | RNA tool | Friction if you reach for... |
|------|----------|------------------------------|
| Find code by intent | `oh_search_context(query)` | Grep for keywords |
| Find symbols by name/kind | `search_symbols(query, kind, ...)` | Grep for definitions |
| Trace dependencies | `graph_query(node, mode: "neighbors")` | Reading imports manually |
| Assess blast radius | `graph_query(node, mode: "impact")` | Guessing from file structure |
| Check outcome alignment | `outcome_progress(outcome_id)` | Reading `.oh/outcomes/` manually |
| Find guardrails | `oh_search_context(query, artifact_types: ["guardrail"])` | Grepping `.oh/guardrails/` |

**The rule:** Try the RNA tool first. If it doesn't work, use whatever does — but log why.
