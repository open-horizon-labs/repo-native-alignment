---
id: broad-lsp-references-require-a-closed-scope-and-shared-circuit
outcome: context-assembly
title: 'Broad LSP References Require a Closed Scope and One Shared Circuit'
---

## Pattern

A broad reference request is bounded only when its declaration resolves to a
closed set of stable node IDs before any language server starts. Changed
files, target symbols, and task-relevant files/symbols are useful scope forms,
but each must fail closed on empty, ambiguous, or unmapped input. A planner
may narrow the declaration; it must never replace it with a root or repository
fallback.

The request/time circuit must also be shared across every participating
language server. A per-profile or per-server limit silently multiplies the
declared budget as languages are added and therefore is not a truthful
request-level boundary.

## Readiness consequence

Completion state and evidence scope are separate dimensions. Durable
readiness must distinguish:

- `default_profile`: repository warm-up completed with broad references
  intentionally omitted;
- `full`: successful repository-wide evidence;
- `scoped`: the declared closed scope completed;
- `partial`: useful output exists, but a circuit, server failure, or
  persistence failure prevented completion;
- `unavailable`: no usable LSP evidence could be produced.

Persist the declaration, limits, actual request count, elapsed time, and
circuit state with the job, then render them through normal MCP readiness
output. A terminal `completed`/`failed` flag alone cannot tell an agent how
much relationship evidence is safe to trust.

## Guardrails

- Broad type/constant references are default-off during repository warm-up.
- One global circuit spans every server in an explicit request.
- Scoped execution never enables repo-wide continuation.
- Deterministic extraction and ordinary search remain available when LSP is
  omitted, partial, or unavailable.

## References

- Issue #769
- `src/extract/lsp/policy.rs`
- `src/server/enrichment.rs`
- `src/server/enrichment_jobs.rs`
- `docs/lsp-enrichment.md`
