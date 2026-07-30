---
session: issue-839
artifact_type: session
updated: 2026-07-30
status: in-progress
outcomes:
  - context-assembly
---

# Issue #839 — bounded warm graph traversal

## Reproduction

The retained rank-9 Django cache was produced cold at commit
`55bcbd8d172b689811fae17cde2f09218dd74e9c` by the qualified RNA bundle at
`13c7453`. The query was:

```bash
repo-native-alignment --business-context=disabled search \
  --repo <retained-django-cache> --compact \
  --node 'django/contrib/admin/sites.py:catch_all_view:function' \
  --mode neighbors
```

| Path | Warm wall time | Output bytes | Graph results |
|---|---:|---:|---:|
| Original producer `search --compact` | 5.32 s | 396,247 | 9 |
| Original producer `graph` (no readiness sidecars) | 2.17 s | 2,306 | 9 |
| #839 default search | 2.35–2.36 s | 1,856 | 9 |
| #839 explicit `--verbose` | 2.46 s | 3,483 | 9 |

The ordinary query therefore removes 55.8% of warm single-process latency and
99.5% of rendered bytes without changing the neighbor result. A normal
55,302-symbol repository cache returns the same exact-node query in 0.51 s
warm. The remaining large-cache startup is graph/process initialization and is
tracked separately by #838's resident-runtime acceptance criteria.

A before/after inventory of every top-level `.oh/.cache` file (path, byte
length, and modification time) was byte-identical across the default query,
confirming that it neither rescans/re-enriches nor mutates query-cache state.

The 2.35 s default figure above is the directly comparable validation against
the original 301,300-node projection. An earlier 45–60 s observation was not a
qualified repeatable measurement and is not used as acceptance evidence. A
five-run component profile with the final binary produced these medians after
discarding each first filesystem/recovery run:

| Cache projection | Graph load (`stats`) | Exact graph traversal | Exact text search |
|---|---:|---:|---:|
| Current repository, 55,026 symbols | 0.63 s | 0.65 s | 1.01 s |
| Recovered Django copy, 103,057 symbols | 1.10 s | 1.21 s | 1.90 s |

The copied Django cache recovered to a 103,057-symbol projection on its first
final-binary access, so those component measurements are not represented as
measurements of the historical 301,300-node projection. The final executable
regression instead persists and reopens a synthetic cache with the exact
historical inventory shape: 301,300 nodes, 535,850 edges, nine neighbors at the
target, and sparse diagnostic sidecars of 147, 176, and 119 MiB. It then invokes
the actual CLI binary against that cache, with a hard 30-second child-process
deadline, and proves that the source sentinel is not rescanned, node/edge counts
do not amplify, the sidecars are not touched, output remains under 8 KiB, and
the nine graph results are preserved. A normal persisted shape (4,947 nodes /
9,537 edges) runs through the same path first. On the M4 Max host the final
phase profile was:

| Phase | Normal shape | Rank-9 shape |
|---|---:|---:|
| CLI process startup (`--version`) | 5.736 s first invocation / 27.8 ms warm | shared process-level phase; cache independent |
| Persisted graph load | 26.9 ms | 1.468 s |
| Exact one-hop graph lookup | 0.027 ms | 0.009 ms |
| Neighbor + edge-evidence rendering | 0.857 ms | 63.6 ms |
| In-process service lookup/render total | 1.83 ms | 127 ms |
| Separate end-to-end warm CLI query | 201 ms | 1.711 s |
| Output | 1,262 bytes | 1,262 bytes |

The direct lookup/render phases use the same production traversal and rendering
helpers as service search. The persisted profile uses production Lance reopen
and the actual CLI, so startup, cache load, lookup/render, and subprocess total
are no longer conflated.

## Root cause

CLI search silently defaulted `verbose=true`. Both CLI and MCP search eagerly
loaded the complete enrichment-job ledger even when diagnostics were not
requested. Verbose rendering also:

- deserialized the 176 MB per-file LSP completeness report and reconciled it
  against the live graph;
- rendered every job; and
- joined every LSP validation record into model-visible output.

For the retained cache, the final validation array alone contained 5,127
records. Diagnostics accounted for almost the entire 396 KB result.

## Inventory diagnosis

The 301,300-node cache is not incremental duplication: its receipt records
`base_cache: null` and `cold_exact_tree: true`. The cold scan produced 111,442
extracted nodes, then LSP consumers produced 189,229 unique virtual nodes and
72,124 LSP edges. Final persistence deduplicated 302,522 intermediate symbols
to 301,300 nodes and stored 535,850 edges, including 27,270 call/reference
edges, across 2,916 files.

The count is fully accounted for, but that does not make every node a useful
traversal entity. In particular, gettext contributed 81,150 virtual nodes with
zero edges and plaintext contributed 6,290 with zero edges. Python contributed
93,987 virtual nodes with 71,584 edges; smaller zero-edge contributors include
CSS (1,618), TypeScript (1,398), JSON (1,563 of 1,586), and others. These may be
legitimate searchable repository entities, but hydrating zero-edge search-only
inventory into every petgraph traversal is a separate product concern. It is
classified and assigned to #844 rather than hidden or deleted in #839.

## Implemented contract

- Search diagnostics are opt-in in both CLI and MCP paths.
- Ordinary search never opens enrichment-job or per-file completeness
  sidecars.
- Verbose search emits at most five job summaries, validation counts rather
  than validation arrays, bounded scope/phase/identifier/failure/evidence
  text, and explicit paths to the full persisted diagnostics.
- Report persistence also writes a fixed-shape completeness summary and
  publishes a small commit marker only after the full report and summary are
  durable. The marker binds the exact summary bytes to the report digest,
  length, and modification identity. The bounded summary also carries the
  producer's exact persisted node-and-edge projection plus the report-identity
  digest. Verbose search compares both to the graph and runtime serving the
  query before reporting ready/degraded status. Report publication, schema
  migration, full/incremental graph writes, and root pruning share a repo-scoped
  cross-process advisory lock. Graph mutations invalidate the publication
  marker before mutation, and competing reports cannot cross-pair their final
  files. Older, interrupted, stale, tampered, oversized, identity-mismatched, or
  graph-mismatched pairs are labeled `status unverified` rather than being
  treated as ready or forcing a full-report read.
- Full observability remains under `.oh/.cache/`; it is not copied into agent
  context.
