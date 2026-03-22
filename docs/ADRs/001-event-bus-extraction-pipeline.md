# RNA Event Bus Architecture
**Status:** Design — pre-implementation
**Issues:** #478 (plugin arch), #479 (event bus), #454 (multi-tenant store)
**Date:** 2026-03-22

---

## Core Principle

Every enrichment stage is an independent `ExtractionConsumer` that subscribes to events it cares about. No consumer knows about any other consumer. The event bus is the only coupling.

## Event Flow

```
Bootstrap (synchronous, per-repo):
  walk roots → RootDiscovered(slug, path)

Consumer of RootDiscovered:
  TreeSitterExtractor           // starts ONCE per root
    → processes all files (rayon internally, O(files) parallel)
    → fires RootExtracted(slug, nodes, edges, delta)

Consumer of RootExtracted:
  LanguageAccumulator           // groups nodes by language
    → fires LanguageDetected(lang, matching_nodes) ONCE per language

Consumer of LanguageDetected(lang, nodes):
  [LSP enrichers — ALL fire concurrently, one per language]
  PyreightEnricher              // starts pyright ONCE, enriches all Python nodes
  TSServerEnricher              // starts tsserver ONCE, enriches all TS nodes
  RustAnalyzerEnricher          // starts rust-analyzer ONCE, enriches all Rust nodes
  → each fires EnrichmentComplete(lang, added_edges, new_nodes, patches)

Consumer of EnrichmentComplete (all languages resolved):
  PostExtractionRegistry        // runs passes once over full merged graph
    ApiLinkPass
    ImportCallsPass
    FrameworkDetectionPass
    TestedByPass
    DirectoryModulePass
    PubSubPass (if framework detected)
    WebSocketPass (if framework detected)
    NextJsRoutingPass (if nextjs detected)
    ...
  → fires PassesComplete(nodes, edges)

Consumer of RootExtracted (streaming, as nodes arrive):
  EmbeddingIndexer              // SINGLETON — embeds nodes as they're extracted
                                // incremental via BLAKE3, doesn't wait for passes
                                // re-embeds any node touched by LSP patches

Consumer of PassesComplete:
  [Singleton consumers — shared across ALL repos]
  LanceDBPersist                // SINGLETON — writes with tenant_id = root slug

// NOTE: Embedding is event-driven per-extraction, not batched at PassesComplete.
// This means vectors are available for search as soon as nodes are extracted,
// not waiting for LSP enrichment or post-extraction passes to finish.
```

## Key Properties

**Parallel LSP** — pyright and tsserver start concurrently as soon as their language is detected. No sequential coupling. RNA repo currently runs pyright then tsserver sequentially; with this architecture both start simultaneously.

**Framework-gated passes** — passes check `detected_frameworks` before running. The registry skips passes whose framework isn't present. Zero cost for repos that don't use that framework.

**Singleton warehouse** — `EmbeddingIndexer` and `LanceDBPersist` are shared across all repos. Multiple per-repo agents can fire `PassesComplete` events; the singletons handle writes with `tenant_id` isolation via existing `lance_write_lock`.

**Incremental by default** — `RootExtracted` carries a delta (new/changed/deleted nodes). Consumers that support incremental (embedding via BLAKE3, LanceDB via merge_insert) only process the delta.

## Bootstrap

```rust
fn bootstrap(workspace: &WorkspaceConfig) -> impl Stream<Item = ExtractionEvent> {
    workspace.resolved_roots()
        .into_iter()
        .map(|root| ExtractionEvent::RootDiscovered {
            slug: root.slug,
            path: root.path,
            lsp_only: root.config.lsp_only,
        })
}
```

One function. No knowledge of languages, extractors, LSP, or passes.

## Consumer Trait

**Pure event subscription — no polling, no context checking.**

A consumer declares which events wake it up. When that event fires, it runs. No `applies_when`, no `ScanContext` — the event itself carries everything the consumer needs.

```rust
trait ExtractionConsumer: Send + Sync {
    fn name(&self) -> &str;
    fn subscribes_to(&self) -> &[ExtractionEventKind];
    fn on_event(&self, event: &ExtractionEvent, bus: &EventBus) -> anyhow::Result<()>;
}

// NextJsRoutingPass wakes up ONLY when FrameworkDetected("nextjs-app-router") fires.
// It never checks context. It never polls. It just reacts.
impl ExtractionConsumer for NextJsRoutingPass {
    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }
    fn on_event(&self, event: &ExtractionEvent, bus: &EventBus) -> anyhow::Result<()> {
        let ExtractionEvent::FrameworkDetected { framework, nodes } = event else { return Ok(()); };
        if framework != "nextjs-app-router" { return Ok(()); }
        let edges = nextjs_routing_pass(nodes);
        bus.emit(ExtractionEvent::PassComplete { name: "nextjs_routing", edges });
        Ok(())
    }
}

// Custom config-driven pass from .oh/extractors/*.toml — same pattern.
// Subscribes to FrameworkDetected for its declared framework.
```

**The framework detection pass is what emits `FrameworkDetected` events:**
```rust
impl ExtractionConsumer for FrameworkDetectionPass {
    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootExtracted]
    }
    fn on_event(&self, event: &ExtractionEvent, bus: &EventBus) -> anyhow::Result<()> {
        // detect frameworks from import nodes
        for framework in detected {
            bus.emit(ExtractionEvent::FrameworkDetected { framework, nodes: matching_nodes });
        }
        Ok(())
    }
}
```

Adding a new enricher = implement `ExtractionConsumer`, declare which event wakes it up. No changes to any other component.

## Multi-Tenant Store (#454)

The singleton `LanceDBPersist` consumer makes multi-tenancy natural:

```
Per-repo agents:                  Singleton consumers:
  RNA agent        ─┐
  IC agent         ─┼─ PassesComplete(tenant_id, ...) → LanceDBPersist (shared DB)
  api-service      ─┘                                  → EmbeddingIndexer (shared)
```

Each repo is a tenant. `tenant_id` = root slug. Multiple repos scanning simultaneously, single store, proper write isolation.

## Migration Path

1. **#478** — Extract `PostExtractionRegistry` (trait + impl). This is the extractor plugin arch.
   Each pass becomes a `PostExtractionPass` implementor with `applies_when()`.

2. **#479** — Introduce `ExtractionEvent` enum and `EventBus`.
   Wrap existing pipeline stages as consumers. No behavior change — just decouple.

3. **Parallel LSP** — Move LSP enrichers to subscribe to `LanguageDetected`.
   Pyright and tsserver fire concurrently. Expected ~2× speedup on multi-language repos.

4. **#454** — Move `LanceDBPersist` and `EmbeddingIndexer` to singleton consumers.
   Centralize store. Per-repo agents become lightweight producers.

## What This Solves

| Problem | Solution |
|---------|----------|
| Sequential LSP (pyright then tsserver) | Both subscribe to `LanguageDetected`, fire concurrently |
| New pass requires touching graph.rs in 4 places | Implement `ExtractionConsumer`, register once |
| Background scanner skips post-extraction passes (#471) | All paths call `run_consumers(PassesComplete)` |
| Upsert delta assembled manually (#474) | Consumers emit deltas as part of their `on_event` return |
| Multi-repo requires separate servers | Singleton consumers accept events from any agent |

## References

- `src/server/graph.rs` — current `build_full_graph_inner` (to be replaced by event flow)
- `src/server/enrichment.rs` — current foreground/background pipeline (to be replaced)
- `src/extract/mod.rs` — current `ExtractorRegistry` (becomes `PostExtractionRegistry`)
- `.oh/sessions/etl-architecture-audit.md` — complexity audit that motivated this
- Issues: #478, #479, #454, #471, #474
