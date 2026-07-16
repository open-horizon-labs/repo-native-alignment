---
title: PR #776 ship friction
date: 2026-07-16
pr: 776
outcome: context-assembly
---

# PR #776 ship friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-16 | Ship pre-flight metis path | minor | `.claude/agents/ship.md` names `.oh/metis/computed-but-not-delivered.md`, while RNA located the canonical promoted artifact at `.oh/guardrails/computed-but-not-delivered.md`. | Reviewed the active guardrail; no delivery-review gap. | Correct the stale path in the ship procedure. |
| 2026-07-16 | RNA scan-gate empty query | minor | The documented empty search is rejected with “Empty query” instead of returning an index count. | A targeted artifact search proved a live 40,554-symbol graph and reported degraded LSP coverage. | Use a supported readiness query in the scan-gate snippet. |
| 2026-07-16 | Exact ship procedure and GitHub diff inspection | skipped | Workflow instructions and the authoritative PR patch require exact text, while the CLI graph does not deliver those complete documents. | Used bounded `sed` and `gh pr diff`; code understanding remained graph-first. | Add exact document/diff delivery to the RNA review path. |
| 2026-07-16 | Independent review source bodies | skipped | RNA returned symbols, signatures, and degraded caller context, but not the complete implementation bodies needed to validate telemetry precedence and proxy trace semantics. | The reviewer used the exact PR diff and targeted source spans, and surfaced five real acceptance gaps. | Make complete changed-function bodies directly available to review agents. |
