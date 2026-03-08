---
id: metis-is-not-universal
outcome: agent-alignment
title: Metis is contextual, not universal — selecting it is the skill
---

The "best practices fallacy": what works in some contexts does not work in all contexts. Metis that is valid for a greenfield Rust project is not valid for a legacy Python monolith. Metis from a solution-space phase does not carry to a problem-space phase. Metis from six months ago may be actively misleading today.

Crucially: **selecting appropriate metis for a given phase and situation is itself a cognitive act**, not a lookup. This is most visible in the solution-space phase, where which prior learnings apply depends on which solution approach is being evaluated. An agent (or human) that pulls all metis indiscriminately and treats it as universally applicable will make worse decisions than one that curates contextually.

**Implication for repo-native-alignment:** The system should make metis *queryable and filterable* — by outcome, by phase, by recency, by tag — but should not automatically apply or weight it. The agent (and human) must select. The harness enables selection; it does not replace it.

**Anti-pattern to avoid:** Auto-consolidating metis across sessions into "universal learnings" strips the contextual provenance that makes the learning meaningful.
