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

## Implemented contract

- `--cache-only` validates an existing business-context marker and persisted
  graph, installs the graph in the resident handler, and fails closed instead
  of scanning or repairing missing state.
- Existing semantic generations open without unpublished scratch state;
  cache-only semantic serving additionally requires the sealed offline bundle.
- Cache-only startup is the single immutable semantic admission boundary: it
  verifies graph/generation identity and both model trees around eager encoder
  and reranker loading before semantic requests are accepted.
- One resident encoder is shared behind an async mutex and the admitted strict
  reranker remains resident across requests. Ordinary non-resident strict mode
  retains its per-call verification contract.
- Query timing emits separate graph load, embedding open, root discovery,
  enrichment-ledger access, encoder wait/initialization/inference, candidate
  retrieval, reranker initialization/inference, full startup validation, and
  resident-reuse phases. Ledger timing and reads occur only for explicit
  verbose diagnostics, including external repos.
- The public HTTP MCP smoke uses three SDK clients after one warmup and asserts
  p95 under 2 seconds, no request over 10 seconds, exact startup/request phase
  counts, and byte/entry/file-or-directory-mtime-identical cache state from
  before startup through shutdown.
- The exact M4 workflow makes the admitted cache and model trees read-only and
  keeps the optional log path outside `.oh/.cache`.

## Local verification after ship-review remediation

- `cargo check --locked --lib`
- `cargo check --locked --lib --bin repo-native-alignment`
- `cargo check --locked --features embeddings,metal,swebench-semantic-bundle --lib --bin repo-native-alignment`
- cache-only external repository rejection before cache creation
- semantic workflow resident-gate contract assertion
- exact-head artifact qualification remains required before release
