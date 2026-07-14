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

- [ ] Reproduce or simulate empty/malformed scorer output after map construction.
- [ ] Add a check that fails direct indexing/downcast panics.
- [ ] Add a check that fails diagnostics containing query, symbol body, or repository path.
- [ ] Add a check that fails unbounded fallback.
- [ ] Add a check that fails graph invalidation after scorer failure.
- [ ] Run `cargo check --lib` before focused and full tests.
- [ ] Run staged `/review`, the project ship gate, real MCP smoke, and CI.
