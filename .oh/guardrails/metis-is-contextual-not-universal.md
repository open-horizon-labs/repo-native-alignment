---
id: metis-is-contextual-not-universal
outcome: agent-alignment
severity: soft
statement: Metis is contextual, not universal — never auto-apply accumulated metis indiscriminately. Selecting appropriate metis for a given phase and situation is itself a cognitive act, not a lookup.
---

## Rationale

What works in one phase/context/task type does not carry universally. Metis from a solution-space phase does not apply to a problem-space phase. Metis from a greenfield Rust project is not valid for a legacy Python monolith. Metis from six months ago may be actively misleading today.

An agent (or human) that pulls all metis indiscriminately and treats it as universally applicable will make worse decisions than one that curates contextually.

The harness enables selection; it does not replace it. `oh_propose_relevant` should weight phase tags heavily — cross-phase metis pollution is a known failure mode.

## Anti-pattern

Auto-consolidating metis across sessions into "universal learnings" strips the contextual provenance that makes the learning meaningful. See: always-on-memory-agent assessment (2026-03-08).

## Evidence
- metis/metis-is-not-universal.md
- metis/llm-synthesis-is-not-judgment.md
- Phase-awareness guardrail in human-led-curation aim

