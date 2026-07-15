---
issue: 721
outcome: context-assembly
phase: execute
---

# Issue 721 execution handoff

## Aim

Keep RNA-first worktree navigation usable when semantic scoring fails: symbol search must return a bounded lexical result, disclose a content-safe scorer diagnostic, and leave the already-built graph intact.

## Selected approach

Harden the existing scorer boundary instead of replacing the search pipeline. Validate embedding and Arrow result shapes inside the embedding component, convert scorer panics or errors into a structured content-safe diagnostic at the service boundary, and preserve the existing bounded lexical fallback.

## Scope

- Embedding scorer input/output validation and diagnostics.
- Service-layer isolation of scorer failure.
- Regression coverage for map-then-search, fallback bounds, diagnostic safety, and graph preservation.

Out of scope: changing embedding models, rebuilding the index format, or introducing a new search abstraction.

## Success criteria

- A live worktree can be mapped and searched in one regression fixture.
- Empty/malformed scorer output cannot crash the request.
- The returned diagnostic names component, model/index state, and fallback without repository content.
- Fallback results are bounded by the requested limit.
- The mapped graph remains queryable after scorer failure.

## Decision basis

The failure is local to the semantic scorer boundary while graph construction and lexical search already work. Preserving those native seams minimizes migration risk and directly satisfies the issue.

Assumptions: scorer failures can be represented without query/source text; lexical candidates are available after a successful map; panic isolation is required because dependency code may still panic.

Accepted trade-off: a degraded lexical result may rank less precisely than semantic search, but remains honest and usable.

Invalidated if the reproduced failure corrupts persisted graph state before scoring. Stop/pivot if isolating the scorer requires replacing the persistence/search stack or if fallback cannot be bounded without discarding valid filters.

## Execution and risk-retirement checklist

- [x] Reproduce or simulate empty/malformed scorer output after map construction.
- [x] Add a check that fails direct indexing/downcast panics.
- [x] Add a check that fails diagnostics containing query, symbol body, or repository path.
- [x] Add a check that fails unbounded fallback.
- [x] Add a check that fails graph invalidation after scorer failure.
- [x] Run `cargo check --lib` before focused tests.
- [x] Run staged `/review`.
- [ ] Run the project ship gate, real MCP smoke, full tests, and CI.

## Execute

### Execution complete

The existing scorer seam now rejects empty query vectors and incompatible Arrow columns as errors. Both code-symbol and artifact scoring execute behind a Tokio task boundary; scorer errors, panics, and cancellations produce a bounded lexical/graph fallback plus a content-safe agent-facing diagnostic. A real temporary worktree is scanned through `build_full_graph` and searched through the MCP handler, while adversarial fixtures prove scorer failure cannot mutate the mapped graph or exceed the requested result limit.

### Verification

- `cargo check --lib`
- `cargo check --lib --features embeddings`
- `cargo test --lib scorer_` (3 passed)
- `cargo test --lib test_symbol_search_after_successful_live_worktree_map` (1 passed)
- `cargo test --lib --features embeddings single_query_embedding_rejects_missing_or_malformed_output` (1 passed)
- `cargo test --lib --features embeddings required_string_column_returns_schema_error_instead_of_panicking` (1 passed)

### Risk retirement

| Risk | Status | Tempting patch failed | Evidence |
|---|---|---|---|
| Empty scorer output still indexes element zero | Retired | Catch only service errors | Shape validation and feature-enabled test |
| Lance result schema still panics | Retired | Guard column presence but keep unchecked downcasts | Typed column helper and feature-enabled test |
| Panic escapes async search | Retired | Handle `Result` only | Tokio task isolation panic fixture |
| Diagnostic leaks repository content | Retired | Echo raw `anyhow` or panic payload | Error and panic redaction fixtures |
| Artifact search re-enters failed scorer | Retired | Isolate only code-symbol search | Shared isolation boundary for both scoring calls |
| Fallback corrupts or replaces mapped graph | Retired | Rebuild/reset graph on scorer failure | Stable-ID preservation and bounded fallback fixture |
| Test claims a map without scanning | Retired | Construct `GraphState` directly | Temp worktree `build_full_graph` plus handler search |

## Review

**Aim:** Keep RNA-first worktree navigation usable and honest when semantic scoring fails.

**Status:** Continue

- Necessary: yes; the unguarded scorer could crash the primary discovery path.
- Aligned: yes; changes remain inside scorer validation, service isolation, and regression coverage.
- Sufficient: yes after two adjustments found in review: artifact scoring now shares the isolation boundary, and the map/search regression uses a real scanner-built graph.
- Mechanism clear: invalid scorer shapes become errors; task failure becomes a safe diagnostic; existing lexical/graph ranking supplies bounded results.
- Risks retired: all model-checkable risks from the handoff have adversarial evidence.
- Frame check: intact. The graph remains valid; the failure belongs at the scorer boundary.
- Drift detected: none. No model, index-format, or search-framework redesign was introduced.

Needs human verification: final delivery through the installed CI artifact and real MCP client remains in the ship gate.
