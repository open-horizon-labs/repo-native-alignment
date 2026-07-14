---
type: session
issue: 728
status: executing
outcome: context-assembly
started: 2026-07-14
---

# Release Tweak — Issue #728

## Aim

Agents can rely on repo-local knowledge after persistence, and the release gate directly proves the delivered custom-edge and local-knowledge behavior.

## Problem Statement

Main commit `dd580f7` passes CI and the artifact-level release suite, but release review found four unresolved PR #727 threads. The symbols store drops node `ExtractionSource`, a hot metadata path performs repeated linear key scans, graph tests violate the indexed-lookup guardrail, and two `.oh/` artifacts lack frontmatter. The release suite also does not directly exercise the #710/#727 path.

## Selected Approach

Fix all four findings in one focused follow-up and add one artifact-level fixture that proves custom relationship labels and local-knowledge metadata survive scan, persistence, load, traversal, and agent-facing search. Preserve legacy tables by defaulting a missing source column to `TreeSitter`.

## Execute

### Scope

- Persist and restore node extraction provenance with a legacy fallback.
- Index typed metadata keys once.
- Replace test graph linear lookups with explicit indexes.
- Add required `.oh/` frontmatter.
- Add explicit release-suite coverage.
- Resolve the four PR #727 threads after evidence is pushed.

### Out of Scope

- Content-native selector/provenance design owned by #711-#715.
- New graph vocabulary or schema beyond node extraction provenance.
- Unrelated dependency, CI, or formatting cleanup.

### Success Criteria

- A Markdown node reloads with `ExtractionSource::Markdown`.
- Legacy symbols tables without the new column still load as `TreeSitter`.
- The hot metadata filter performs indexed membership checks.
- No changed graph test uses `.iter().find()`.
- Both affected `.oh/` files have valid frontmatter.
- `scripts/test-suite.sh` proves the delivered #710/#727 behavior.
- Targeted tests, full tests, Clippy, ship, and release review pass.

### Risk Retirement

| Risk | Adversarial check |
|---|---|
| New source column breaks legacy caches | Load a table/schema without the column and assert `TreeSitter` fallback |
| Source is written but not restored | Persist a Markdown node, reload it, assert `Markdown` |
| Custom metadata disappears during schema work | Existing metadata round-trip plus artifact-level local-knowledge assertions |
| HashSet is rebuilt per node | Reusable static index initialized once |
| Release test only checks source text | Scan a fixture and query the persisted graph with the release artifact |

### Stop/Pivot Triggers

- Pause if the schema change requires destructive migration.
- Pause if release coverage cannot run against the exact CI artifact.
- Pause if a fix conflicts with #711-#715 content-native contracts.

## Dissent

### Decision

**Recommendation:** PROCEED

The strongest alternative is to hide node provenance in `metadata_json` and avoid a schema bump. That is easier, but not simpler: it weakens the typed persistence contract, duplicates a first-class `Node` field into an extensibility blob, and violates the computed-but-not-delivered guardrail.

### Contrary Evidence and Adjustments

- A new non-null column would make direct loading of legacy tables fail. Use a nullable column and `column_by_name` fallback.
- A schema bump discards Lance data. This is acceptable because Lance is a rebuildable cache and `.oh/` plus source files remain authoritative; it is the repository's established migration mechanism.
- String encodings can drift. Reuse the existing `parse_extraction_source` vocabulary and add round-trip coverage for every variant.
- The active content-native worktree already carries schema-version work. Do not edit it; its eventual rebase must take the next available version if #728 lands first.

### Pre-Mortem

1. **Functional:** source is written but loaders still default to TreeSitter. The Markdown persist/load assertion fails.
2. **Compatibility:** a legacy table lacks the column and cold start panics. The legacy-schema load regression fails.
3. **Opportunity cost:** the fix expands into #711 provenance semantics. Scope checks reject edge/body-evidence changes.

No ADR is required: the change follows the existing typed-column and schema-version pattern for an internal, rebuildable cache rather than introducing a new architectural boundary.

### Execute Result

- Added nullable typed node-provenance persistence with a legacy `TreeSitter` fallback and schema version 23.
- Replaced repeated typed-key scans with one static `HashSet` and changed local-knowledge graph tests to indexed lookups.
- Added missing frontmatter to the two #709 artifacts.
- Added release-fixture coverage for persisted `supports` and `consumes` edges plus agent-visible source metadata.
- `cargo check --lib`: pass.
- `cargo test --lib server::store`: 19 passed.
- `cargo test --lib local_knowledge`: 3 passed.
- Candidate-binary `scripts/test-suite.sh`: 141 passed, 0 failed, 0 skipped.

Implementation is ready for the full PR #729 ship gate.
