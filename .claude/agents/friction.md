---
name: friction
description: Log RNA MCP tool friction. Use whenever an RNA tool fails, frustrates, or gets skipped in favor of Grep/Read.
---

# /friction

Log a friction event with an RNA MCP tool. This is how the product improves — every friction point is signal.

## The Rule

**Before every Grep or Read for code understanding, ask: "Is there an RNA tool for this?"**

- If yes and you use it → great, no log needed (unless it fails)
- If yes and it fails → log the failure
- If yes and you skip it → **log why you skipped it** (this IS friction)
- If no RNA tool applies → no log needed

**Using Grep/Read for code understanding without trying the RNA tool first is itself a friction event.** Not trying is not "no friction" — it's the worst kind of friction, because it means the tool wasn't even worth attempting. That's critical signal.

The only Grep/Read uses that are NOT friction events:
- Searching for non-code content (config files, CI yaml, shell scripts)
- Reading a specific file you already know the path to (e.g., after RNA told you the file)
- Searching for string literals, error messages, or log output
- Grep/Read that RNA genuinely can't do (regex patterns, multi-file sed-like operations)

## When to Log

Call `/friction` (or apply this process inline) whenever:

- An RNA tool returned **wrong or irrelevant results**
- An RNA tool was **missing data** you expected (symbol, edge, metadata)
- The API was **awkward** — needed N calls where 1 should suffice, params confusing
- A tool was **too slow** and disrupted your flow
- You **used Grep/Read instead of an RNA tool** — log which RNA tool you should have used and why you didn't
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

**For skipped-RNA events, use this format:**

| Phase/Step | Tool | What happened | Workaround | Severity |
|------------|------|---------------|------------|----------|
| Phase 3 | search_symbols (skipped) | Needed to find all functions in embed.rs | Used Grep for "fn " | skipped |

Also append to the active session file's `## RNA Tool Friction Log` table if one exists — this keeps friction visible in the session while the canonical log lives in `.oh/friction-logs/`.

**Severity levels:**
- **blocker** — had to abandon the tool entirely, couldn't complete the task with it
- **friction** — worked around it, but it cost time or context window
- **papercut** — minor annoyance, still usable
- **skipped** — didn't try the RNA tool at all; used Grep/Read instead

## Friction Summary

At the end of a pipeline run (or on explicit `/friction summary`), review the accumulated friction log and produce:

```markdown
## Friction Summary

**Total events:** N (X blockers, Y friction, Z papercuts, W skipped)
**RNA adoption rate:** X RNA calls / (X RNA calls + W skipped) = N%

### Patterns
- <pattern 1>: <description> (N occurrences)
- <pattern 2>: <description> (N occurrences)

### Skipped — Why?
- <reason 1>: (N occurrences) — e.g., "didn't think of it", "Grep felt faster", "wasn't sure which RNA tool"
- <reason 2>: (N occurrences)

### Recommendations
- **File issue:** <description> — severity warrants tracking
- **Update existing issue:** #<number> — <how this friction relates>
- **Known limitation:** <description> — not actionable yet, note for context
```

The `skipped` count and adoption rate are the most important metrics. A pipeline with 0 friction events and 30 Grep calls isn't frictionless — it's unmonitored.

## RNA Tool Quick Reference

**Before using Grep or Read for code understanding, check this table.**

| Need | RNA tool to try FIRST | Grep/Read is friction if... |
|------|----------------------|---------------------------|
| Find code by intent | `search(query)` | You Grep for keywords instead |
| Find symbols by name/kind | `search_symbols(query, kind, ...)` | You Grep for `fn `, `struct `, `impl ` |
| Trace dependencies | `graph_query(node, mode: "neighbors")` | You Read imports manually |
| Assess blast radius | `graph_query(node, mode: "impact")` | You guess from file structure |
| Check outcome alignment | `outcome_progress(outcome_id)` | You Read `.oh/outcomes/` manually |
| Find guardrails | `search(query, artifact_types=["guardrail"])` | You Grep `.oh/guardrails/` |
| Understand a function's role | `graph_query(node, mode: "neighbors")` | You Read the file and scan context |

**The rule:** Try the RNA tool first. If it doesn't work, use whatever does — but log why. If you don't try it, log why not.
