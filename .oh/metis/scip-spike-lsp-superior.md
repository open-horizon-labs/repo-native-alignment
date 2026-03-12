---
id: scip-spike-lsp-superior
outcome: agent-alignment
title: SCIP spike concluded LSP provides the same edges without build-time indexers
---

## What Happened

PR #114 spiked SCIP (Source Code Intelligence Protocol) as a third enrichment tier alongside tree-sitter and LSP. The enricher worked — it extracted Calls and Implements edges with compiler-grade confidence from rust-analyzer, scip-python, scip-typescript, and scip-go.

But SCIP and LSP produce the same semantic edges (call hierarchy, type hierarchy, implements) because SCIP indexers are often the same tools as LSP servers, run in batch mode instead of live. SCIP adds: a separate indexer binary per language, a batch index step producing a protobuf file, and parsing/cleanup of that file. LSP servers are already on most developers' PATH (editors use them), start on demand, and return the same edges through a standard protocol.

## Decision

Closed #114 without merging. Salvaged 4 reusable patterns into #122 (process timeout, file-line index, edge deduplication, HEAD-based cache invalidation). The SCIP enricher itself was unnecessary.

## Why This Matters

The docs drifted after this decision — the comparison doc said SCIP was "Planned" for months after the spike concluded it wasn't needed. This happened because the decision wasn't captured as metis at the time. Uncaptured decisions rot into stale docs.

## Broader Pattern

When you spike something and decide against it, record the decision immediately. The code gets closed/reverted, but the *reasoning* has no home unless you write it down. Future sessions (and docs) will assume the feature is still planned unless told otherwise.

## Evidence Source

- PR #114 (closed): SCIP enricher spike
- PR #122 (merged): salvaged patterns from #114
