# ArcSwap for Graph Concurrency
**Status:** Implementing (issue #574)
**Date:** 2026-03-24

---

## Context

The RNA MCP server serves tool calls (search, graph_query, list_roots) from an in-memory graph while a background scanner continuously re-extracts and enriches it. The event bus pipeline (ADR-001) runs LSP enrichment, post-extraction passes, and PageRank — taking 10-30+ seconds on non-trivial codebases.

The original implementation used `Arc<RwLock<Option<GraphState>>>`. The background scanner acquired a write lock for the entire enrichment pipeline, blocking all tool calls for its duration. On a repo like Innovation-Connector (87K nodes), tool calls hung for 2+ minutes. This caused v0.1.15 and v0.1.16 to be yanked.

An intermediate fix (3-phase lock splitting) was attempted and rejected — it added intricate multi-phase coordination for marginal benefit and failed to compile cleanly.

## Decision

Replace `Arc<RwLock<Option<GraphState>>>` with `Arc<ArcSwap<Option<Arc<GraphState>>>>`.

- **Readers** (tool calls): `graph.load()` — atomic pointer load, zero blocking, always sees the last complete graph.
- **Writer** (background scanner): loads current graph → clones → applies file changes + enrichment to the clone → `graph.store(new_graph)` — atomic pointer swap.

This is the RCU (Read-Copy-Update) pattern. ArcSwap is the standard Rust implementation (130M+ downloads, used by tokio/tracing/hyper).

## Why not RwLock?

RwLock is writer-preferred in tokio: once a writer is queued, all new readers block behind it. This is wrong for RNA's access pattern where readers (tool calls) must never block and writers (enrichment) are infrequent but slow.

## Why not the 3-phase lock split?

The enrichment pipeline takes ownership of nodes/edges via `std::mem::take`, leaving the graph empty during processing. Splitting the lock into brief-lock → no-lock → brief-lock requires either cloning the graph anyway (same as ArcSwap but with more lock coordination) or leaving readers seeing an empty graph during Phase 2. All complexity, no benefit over ArcSwap.

## Consequences

- Tool calls never block, regardless of enrichment duration
- During enrichment, tool calls see the previous complete graph (slightly stale but consistent)
- Memory doubles briefly during enrichment (old + new graph) — acceptable on modern machines
- `GraphState` must be wrapped in `Arc` for atomic swapping
- ArcSwap was previously used in this codebase and proven to work

## References

- Issue #574
- `arc-swap` crate: https://docs.rs/arc-swap
- ADR-001 (event bus pipeline that produces the enrichment workload)
