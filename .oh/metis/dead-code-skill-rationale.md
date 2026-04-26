---
id: dead-code-skill-rationale
outcome: context-assembly
title: 'Dead-Code Lives in Skills, Not in Code: Graph Query, Not Compiler Pass'
---

## Pattern

Detecting dead code in a multi-language repo is a **graph query**, not a code change. RNA already extracts `Calls` (from LSP callHierarchy) and `ReferencedBy` (from LSP textDocument/references) edges into the graph. A function with no incoming non-test edges is a candidate. That's a query, not an extraction pass — so it belongs in `plugin/skills/dead-code/SKILL.md`, not in a new `dead_code_pass` in `src/extract/`.

## The trigger that produced this

PR #643 introduced the `/rna-mcp:dead-code` skill. The shape that earned its place in `plugin/skills/` rather than `src/`:

- It produces no new graph data. It only **traverses** existing edges.
- Its judgement is heuristic — false positives are unavoidable (framework callbacks, trait impls, FFI exports, CLI dispatch targets, re-exports). A compiler-style pass would have to encode every framework's calling convention.
- The agent surfaces candidates with confidence levels and a human/agent decides what to delete. That's a workflow concern, not a runtime concern.

A dogfood session shipped one bug-class with the skill: `In: 1` does *not* mean "one caller." The single incoming edge is the parent module/struct's `Defines` relationship. A function with `In: 1` and only a `Defines` edge has **zero callers**. True "has one caller" is `In: 2` (one `Defines` + one `Calls`). The skill captures this in Step 2 so future runs don't waste a query roundtrip.

## Why this matters

Putting dead-code in `src/` would have:
- Coupled query logic to the extraction pipeline (an ADR-001 violation: pipeline emits edges, queries consume them).
- Forced false-positive heuristics into Rust, where they would calcify.
- Required a new MCP tool surface for what the existing `search(node, mode="neighbors", direction="incoming")` already does.

Putting it in `plugin/skills/` keeps the heuristic visible to the agent that runs it, which is also the agent best positioned to recognize "that's a `#[tokio::main]`, not dead code."

## LSP caveat (the hard prerequisite)

The skill's accuracy is bounded by the graph it queries. If LSP enrichment did not run (or aborted), the graph has only structural edges (`Defines`, `Contains`, `BelongsTo`) — no `Calls`, no `ReferencedBy`. Every function then looks dead. The skill's Step 0 verifies LSP enrichment completed before any candidate analysis; if call-edge counts are zero in the scan output, the skill aborts with a clear message rather than producing a false-positive avalanche.

Run order for trustworthy results:
```
repo-native-alignment scan --repo <path> --full
# Confirm scan output reports non-zero LSP call edges before invoking /dead-code.
```

## When to invoke

- Periodic repo hygiene before a release — sweep candidates, confirm with the team, delete in a separate PR.
- After a refactor that removed a caller — find chains that became dead.
- When CI test coverage drops on a file — sometimes the file is dead, not undertested.

## When not to invoke

- On a freshly-cloned repo before `scan --full` finishes.
- On Python/TS-only repos where `pyright`/`tsserver` is not on PATH (LSP enrichment will be skipped silently).
- For dynamically-dispatched code (trait objects, function pointers, reflection) — the graph cannot represent those edges and will overestimate dead candidates.

## Discoverability commitment (#650)

Until v0.2.7, the skill existed but neither `README.md` nor `AGENTS.md` mentioned it. An agent grepping repo metadata could not discover it. Closing #650 added:

- `README.md` — bullet under "Plugin Skills" linking to `plugin/skills/dead-code/SKILL.md` with the LSP-required caveat.
- `AGENTS.md` — bullet under "Plugin Skills (this repo)" with the `In: 1 = zero callers` gotcha inline.
- This metis — rationale + caveat for future curation.

If a new skill ships without a paired README/AGENTS surface and a metis like this one, it is **not discoverable**, full stop.

## References

- Skill: `plugin/skills/dead-code/SKILL.md`
- PR #643: introduced the skill
- PR #644: skill quality findings (Step 0 self-checking, hooks matcher tightening) folded into #650
- Issue #650: discoverability + skill quality umbrella
- Guardrail: `.oh/guardrails/dogfood-rna-tools.md` (every Grep/Read fallback for skill discovery is a friction event)
