---
issues:
- https://github.com/open-horizon-labs/repo-native-alignment/issues/669
- https://github.com/open-horizon-labs/repo-native-alignment/issues/670
- https://github.com/open-horizon-labs/repo-native-alignment/issues/671
outcome: context-assembly
phase: execute
updated: 2026-05-05
---

# OperationReport Option D Execute

## Pre-flight
**Updated:** 2026-05-05
**Status:** in-progress

- [x] Aim is clear — make RNA operationally observable and self-diagnosing for scan/enrich/readiness flows.
- [x] Constraints known — extend ADR-003, do not introduce readiness booleans, stay repo-native, no external telemetry, preserve existing scan/enrich behavior.
- [x] Context loaded — issues #669-#671 define model, rendering/timings, and bounded persistence/MCP/status exposure.
- [x] Scope bounded — one PR for #669-#671; implement OperationReport model, wire scan/enrich summaries/timings, persist recent history, expose through status/list-roots/search-equivalent surfaces as justified. Do not redesign scheduler or extraction semantics.
- [x] Success criteria — acceptance criteria on #669/#670/#671 pass with tests, README/ADR updated, draft PR exists before code, verification passes.

## Drift guard
If persistence or MCP exposure requires a new scheduler/tool protocol redesign, pause and narrow to OperationReport + CLI/list_roots exposure rather than expanding beyond Option D.
