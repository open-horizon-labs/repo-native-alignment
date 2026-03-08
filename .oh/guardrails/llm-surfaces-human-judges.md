---
id: llm-surfaces-human-judges
outcome: agent-alignment
severity: soft
statement: LLMs surface candidates; humans judge what matters. Never auto-promote metis to guardrails, auto-consolidate learnings, or auto-apply accumulated context without a human selection step.
---

## Rationale

LLMs are good at extraction — surfacing themes, grouping similar items, summarizing patterns. LLMs are not reliable at judgment — deciding what is significant, what should govern future behavior, what is worth promoting to a guardrail.

Systems that automate the judgment step create false confidence: the store fills with LLM-generated "insights" that look authoritative but reflect no actual situated experience.

## The Point

The human-curation step in the metis→guardrail pipeline is not an inefficiency to automate away — it is the point. This distill pass itself is the mechanism: the agent surfaces candidates, the human (user) decides to promote.

## Override Protocol

The human can explicitly instruct promotion ("promote that one because I said so") — that IS the human judgment step exercised. The guardrail prohibits *automated* promotion, not human-directed promotion.

## Evidence
- metis/llm-synthesis-is-not-judgment.md
- always-on-memory-agent external exploration (2026-03-08)
- This very distill pass: user overrode the "let it sit" rule — correct use of human judgment

