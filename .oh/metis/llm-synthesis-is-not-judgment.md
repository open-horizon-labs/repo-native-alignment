---
id: llm-synthesis-is-not-judgment
outcome: agent-alignment
title: LLMs can extract themes; they cannot judge what matters
---

The always-on-memory-agent auto-consolidates memories via LLM and promotes insights to a persistent store. This conflates two distinct acts: **extraction** (LLMs are good at this — surfacing themes, grouping similar items, summarizing patterns) and **judgment** (LLMs are not reliable at this — deciding what is significant, what should govern future behavior, what is worth promoting to a guardrail).

Deciding what metis is worth keeping, and which guardrails to promote, requires human judgment. Systems that automate this step create false confidence: the store fills with LLM-generated "insights" that look authoritative but reflect no actual situated experience.

**Implication for repo-native-alignment:** The human-curation step in the metis→guardrail pipeline is not an inefficiency to automate away — it is the point. LLMs can surface candidates. Humans decide.
