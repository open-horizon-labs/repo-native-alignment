---
id: human-led-curation
status: proposed
mechanism: |-
  LLM-assisted tools that reduce the cognitive load of finding signal in an accumulating
  corpus of metis and guardrails. LLMs surface candidates; humans judge what matters.

  Deferred until manual curation (via /distill sessions) proves insufficient at the
  current corpus growth rate. Solo developer context bounds the accumulation rate.
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

## Complementary to agent-local notes

Agent tools like Cursor rules and Cline rules accumulate session-local, auto-written notes. RNA metis is different: cross-session, cross-agent, curated by a human who decided it was worth remembering. The distinction: agent notes are a scratchpad; RNA metis is institutional memory.

These are complementary layers, not competitors. An agent might write its own note about a specific file's quirk, and separately benefit from RNA metis about a systemic pattern. The value of RNA metis comes from the human judgment step — it's filtered, not accumulated.

## The Two Tools

### propose_themes (via `/distill` skill)
- **Input:** metis corpus (all or filtered), optional focus (outcome/phase/tag)
- **Process:** cluster by semantic similarity, extract recurring themes, surface metis IDs
- **Output:** proposed theme groups with supporting entries, ranked by frequency/recency
- **Human action:** review proposals, select what to compact or promote to guardrails

### Contextual retrieval (via `search`)
- `search(artifact_types=["metis"])` — query metis by relevance to current work
- `search(artifact_types=["guardrail"])` — find applicable constraints
- Agents reach for these when the skills prompt them to, or spontaneously

## Guardrails

- **Never auto-apply:** proposals are candidates, not decisions
- **Never auto-promote:** a theme proposal is not a guardrail until a human writes it
- **Preserve provenance:** each proposal links to source metis IDs
- **Phase-aware:** oh_propose_relevant must weight phase tags heavily — cross-phase metis
  pollution is a known failure mode

## Open question (#360)
Agents can retrieve metis via `search(artifact_types=["metis"])`, but do they? The business context preamble already surfaces guardrails. Should relevant metis also appear there? And what does "agents are using metis" look like as an observable behavior?

## Signals
- Human selects ≥1 proposed item in >50% of `/distill` invocations (proposals are useful)
- Human dismisses all items in <20% of invocations (proposals are not noise)
- Agents reference metis IDs in their reasoning (observable in session logs)
