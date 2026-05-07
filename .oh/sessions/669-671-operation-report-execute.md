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


## Execute
**Updated:** 2026-05-05
**Status:** complete

Implemented Option D in one PR branch:
- #669: Added `OperationReport` model, structured capability/phase/degradation/next-step/diagnostic reports, CLI and markdown renderers, bounded store, ADR-004, and tests.
- #670: Added `scan --timings`; scan/full/incremental/cache/extract-only/explicit enrich paths now create/persist/render operation reports with degraded query classes and next enrichment commands.
- #671: Persisted bounded recent operation history at `.oh/.cache/operation_reports.json`; `list_roots`/MCP-visible root output appends recent operation reports; stale non-terminal records are marked stale on read.

Verification completed:
- `cargo check --lib --bins --no-default-features`
- `cargo clippy --lib --bins --no-default-features -- -D warnings`
- `cargo test --lib --no-default-features operation_report -- --nocapture`
- `cargo test --lib --no-default-features test_list_roots_from_slugs_includes_recent_operation_reports -- --nocapture`
- `cargo test --lib --no-default-features -- --test-threads=1`
- Manual smoke with built binary: `scan --extract-only --no-embed --no-lsp --timings`, persisted report JSON assertion, and `list-roots` recent operation output.

Observed test-suite note: default parallel `cargo test --lib --no-default-features` repeatedly exposed an existing order/concurrency-sensitive failure in `server::tests::test_declared_root_persists_across_fresh_handler_scans`; the test passes in isolation and the full suite passes with `--test-threads=1`.