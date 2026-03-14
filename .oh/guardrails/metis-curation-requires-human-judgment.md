---
id: metis-curation-requires-human-judgment
outcome: agent-alignment
severity: soft
statement: LLMs surface candidates; humans judge what matters. Never auto-promote, auto-consolidate, or auto-apply metis without a human selection step. Metis is contextual — what worked in one phase/context does not carry universally.
---

## Two Rules, One Principle

**1. No automated promotion.** LLMs extract themes and group similar items. They do not decide what is significant, what should govern future behavior, or what is worth promoting to a guardrail. The human-curation step is the point, not an inefficiency to automate away.

**2. No indiscriminate application.** Metis from a solution-space phase does not apply to a problem-space phase. Metis from a greenfield Rust project is not valid for a legacy Python monolith. An agent that pulls all metis and treats it as universal will make worse decisions than one that curates contextually.

## Override Protocol

The human can explicitly direct promotion ("promote that one") — that IS the human judgment step exercised. The guardrail prohibits *automated* promotion and *indiscriminate* application, not human-directed decisions.

## Evidence
- metis/llm-synthesis-is-not-judgment.md
- metis/metis-is-not-universal.md
- always-on-memory-agent external exploration (2026-03-08)
- Phase-awareness guardrail in human-led-curation outcome
