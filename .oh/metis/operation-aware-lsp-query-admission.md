---
id: operation-aware-lsp-query-admission
outcome: context-assembly
title: 'Admit LSP Work by Semantic Operation, Then Measure Its Yield'
---

## Pattern

An AST declaration kind is not enough evidence to schedule an LSP request. Admission should combine the semantic operation, declaration class, language/server profile, negotiated capability, and current runtime budget before a work item enters the queue.

The same semantic operation should key telemetry. Counting scheduled RPCs, non-empty responses, emitted edges, latency, timeouts, and errors by operation and declaration class makes expensive or low-yield profiles visible instead of letting a broad allow-list quietly grow.

## Why this matters

Language servers vary by both capability and behavior. A server advertising references does not imply that references for every extractable kind are useful or bounded. Function call hierarchy and trait implementations are high-signal graph relationships; broad constant references are default-denied until measurement earns their inclusion.

Keep the profile at the shared LSP construction seam and apply it before every relevant scheduling pass. Deliver the resulting measurements through the same event and status path agents already inspect; telemetry computed only inside the enricher cannot guide future policy.

## Guardrails

- Synthetic graph values are searchable evidence, never compiler declarations or LSP request targets.
- Language-specific restrictions belong in `LangConfig` or the shared profile, not generic `if language == ...` branches.
- A negotiated server capability is necessary but not sufficient for admission.
- Runtime budgets are part of admission even when current built-in profiles use unlimited defaults.

## References

- Issue #767
- `src/extract/lsp/policy.rs`
- `docs/lsp-enrichment.md`
