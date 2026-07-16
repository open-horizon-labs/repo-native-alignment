---
session: 780-execute
artifact_type: session
updated: 2026-07-16
---

# Issue #779 — paired SWE-bench Verified pilot

## Execute

**Status:** in-progress

### Pre-flight

- Aim: produce auditable paired causal evidence for one immutable published small model.
- Constraints: arms differ only by RNA availability; exact CI artifact; real pre-edit MCP tool calls; official evaluator; unsuccessful runs retained; no full-suite comparison for the subset pilot.
- Context: #770/#776 one-instance child harness, untracked one-instance metis, issue #779 acceptance criteria, RNA structural graph.
- Scope: add paired orchestration, parity validation, aggregation/reporting, tests/docs, and retain the real paired pilot bundle by immutable references. No multi-model framework.
- Success: a frozen real task population completes in both arms through official evaluation and the complete `/ship` gate.

### Selected design

Wrap the landed one-instance harness with a paired orchestrator. Reuse the child bundle contract, add an explicit baseline child mode that disables RNA preparation/MCP without changing executor input, validate all controlled fields centrally, and aggregate per-instance results and context/cost telemetry without inferring missing values.
