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
