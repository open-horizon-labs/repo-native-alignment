---
id: deterministic-bootstrap-over-packaging
outcome: agent-alignment
title: Install friction needs deterministic bootstrap, not packaging alone
---

Solution-space exploration for install/init clarified that packaging alone does not solve adoption reliability.

What changed:
- Framing shifted from "make install easier" to "make first-run success deterministic and measurable".
- Selected option: idempotent bootstrap flow (`install + init + verify`) as the minimum approach that addresses config wiring, dependency preflight (`protoc`), and real-client validation.

Why it matters:
- Preserves lightweight/repo-native guardrails while avoiding another docs-only loop.
- Creates a measurable funnel (success rate, time-to-first-tool-call) before investing in heavier CLI redesign.
