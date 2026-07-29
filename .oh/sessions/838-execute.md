---
session: issue-838
artifact_type: session
updated: 2026-07-29
status: in-progress
outcomes:
  - context-assembly
---

# Issue #838 — resident cache-backed query runtime

## Problem

Cache-backed queries pay process startup and graph/index/model initialization
per request. The existing MCP server has resident state, but startup currently
loads a cached graph and discards the returned `GraphState`, so the first tool
call can still enter graph construction.

## Solution-space decision

Use the public HTTP MCP server as the reusable query runtime. Do not add a
second daemon or private query protocol.

- Store admitted graph and embedding state at startup.
- Add an explicit cache-only mode that fails closed instead of scanning,
  enriching, downloading, or mutating the cache.
- Instrument graph load, embedding open, root/ledger access, query encoding,
  retrieval, and reranker initialization/inference.
- Qualify the public transport after one warmup with three concurrent real MCP
  clients and assert interactive latency plus unchanged cache state.
- Add regressions that fail if startup discards the graph or cache-only requests
  enter construction/update paths.

## Rejected approaches

- Larger harness timeouts preserve the cold-start defect.
- Process-per-query CLI optimization still multiplies model memory and startup.
- A new daemon duplicates the MCP lifecycle and state-management surface.

## Constraint

The implementation must preserve deterministic query bytes and exact persisted
cache admission. It must not weaken semantic qualification or silently rebuild
an invalid/missing cache.
