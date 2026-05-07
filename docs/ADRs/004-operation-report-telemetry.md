# ADR-004: OperationReport telemetry control plane

## Status

Accepted

## Context

ADR-003 made enrichment capability readiness durable by recording capability-scoped jobs instead of collapsing readiness into a boolean. That solved one part of the problem: agents can see whether embeddings or call-reference enrichment are running, complete, failed, timed out, or superseded.

The remaining Option D gap is broader. Scan, incremental refresh, cache load, explicit enrich, startup/background work, CLI summaries, and MCP/status surfaces need one report model for:

- what operation ran;
- which phases ran, skipped, failed, or were unavailable;
- which capabilities are trustworthy;
- which query classes are degraded;
- what command should run next;
- what happened recently after a restart or failed run.

Ad hoc CLI strings are not enough. If timing and readiness are computed only in `main.rs`, MCP tools cannot deliver the same facts and the project reintroduces capability drift.

## Decision

Introduce `OperationReport` as the canonical operation-level telemetry model.

`OperationReport` records:

- operation kind and trigger;
- structured operation state;
- measured phase reports;
- structured capability reports;
- output counters;
- degradation notices by query class;
- next-step commands;
- diagnostics;
- related enrichment job IDs.

Operation reports are rendered through reusable renderers instead of bespoke CLI strings. The first renderers are human CLI text and markdown/MCP-friendly text.

Recent reports are persisted under `.oh/.cache/operation_reports.json` as bounded diagnostic/control-plane state. This file is not source truth; it is a repo-native history of recent operations. Non-terminal persisted operations are marked stale on read because a previous process likely exited before completing them.

ADR-003 remains the capability job ledger. Operation reports complement it and link to enrichment job IDs where applicable. They must not create a second scheduler or conflicting readiness source.

## Consequences

- CLI scan/enrich summaries can report skipped phases, degraded query classes, next steps, and timings from the same model used by MCP/status surfaces.
- `list_roots` can show recent operation history without requiring users or agents to read logs.
- Timing output is limited to phases that are measured at an owning boundary. Unmeasured subphases are omitted rather than inferred.
- Future status/doctor surfaces should consume OperationReport history instead of inventing new readiness strings.

## Guardrails

- Do not introduce `ready: bool` as source truth for capabilities.
- Do not persist unbounded telemetry; keep operation history bounded and repo-native.
- Do not make OperationReport a second enrichment scheduler.
- Do not claim phase timings that are not measured at the owning boundary.
