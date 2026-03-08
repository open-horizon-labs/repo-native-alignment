---
id: human-led-curation
status: proposed
mechanism: |-
  Two MCP tools that assist human judgment without replacing it:

  **oh_propose_themes:** Given accumulated metis entries (optionally filtered by outcome,
  phase, or tag), extract recurring patterns and propose candidates for promotion to
  guardrails or compaction. Returns a ranked list of themes with supporting metis IDs.
  Human reviews, selects what to promote, discards the rest.

  **oh_propose_relevant:** Given current task description, phase, and active outcome,
  rank and filter the metis+guardrails corpus by likely relevance. Returns a short
  candidate list with reasoning. Human selects what to load into context.

  Neither tool makes decisions. Both reduce the cognitive load of finding signal in
  an accumulating corpus — which is exactly where LLMs have leverage without needing judgment.
files:
- src/query.rs
- .oh/metis/*
- .oh/guardrails/*
- .oh/outcomes/*
---

# Human-Led Curation with LLM Assistance

The human remains the judge of what matters. LLMs reduce the cognitive load of pattern
recognition and relevance filtering in a growing corpus of metis and guardrails.

## Why This Matters

Metis accumulates. After 20+ sessions, no human can hold all of it in mind to decide:
"what patterns are recurring? what's worth promoting to a guardrail? what applies here?"
This is not a judgment problem — it's a *search and surface* problem. LLMs are good at
that. The judgment (what to keep, what to promote, what to apply) stays human.

## The Two Tools

### oh_propose_themes
- **Input:** metis corpus (all or filtered), optional focus (outcome/phase/tag)
- **Process:** cluster by semantic similarity, extract recurring themes, surface metis IDs
- **Output:** proposed theme groups with supporting entries, ranked by frequency/recency
- **Human action:** review proposals, select what to compact or promote to guardrails

### oh_propose_relevant
- **Input:** current task description + active outcome + current phase
- **Process:** semantic search + phase-tag filtering → ranked candidates
- **Output:** short list (5-10) of metis/guardrails with relevance reasoning
- **Human action:** select what to load into context; dismiss the rest

## Guardrails

- **Never auto-apply:** proposals are candidates, not decisions
- **Never auto-promote:** a theme proposal is not a guardrail until a human writes it
- **Preserve provenance:** each proposal links to source metis IDs
- **Phase-aware:** oh_propose_relevant must weight phase tags heavily — cross-phase metis
  pollution is a known failure mode

## Signals
- Human selects ≥1 proposed item in >50% of invocations (proposals are useful)
- Human dismisses all items in <20% of invocations (proposals are not noise)
