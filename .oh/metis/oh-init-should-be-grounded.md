---
id: oh-init-should-be-grounded
outcome: agent-alignment
title: oh_init should scaffold from OH graph + project context, not empty templates
---

## What Happened

This entire session was a manual oh_init: reading the project, querying OH, writing outcomes/signals/guardrails/metis by hand over hours. The oh_init tool we built creates empty templates with placeholder text.

## The Learning

oh_init should do in 10 seconds what we did in a session:
1. Read CLAUDE.md, README, Cargo.toml for project context
2. Query OH graph (oh_get_endeavors) to find matching aims for this repo
3. Pull aim description as the outcome body
4. Extract file patterns from the actual source tree
5. Create signals from the aim's declared feedback signals
6. Seed guardrails from OH graph guardrails for this endeavor
7. Write CLAUDE.md workflow integration section (skills + MCP mapping)

Empty templates are a cold-start trap. Grounded scaffolding from existing context is the fix. The compound effect starts from commit 1 instead of session 5.
