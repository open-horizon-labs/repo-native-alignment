# LSP Enrichment

RNA auto-discovers installed language servers and enriches the graph with cross-file edges. No configuration -- if the binary is on PATH, it's used. Missing servers are skipped gracefully.

## What LSP Adds Beyond tree-sitter

- **Who calls this?** -- inbound call graph, not just the function definition
- **What does this call?** -- outbound call chain, including into external packages (`tokio`, `lancedb`, your dependencies)
- **Who implements this trait/interface?** -- implementation edges across files
- **Doc cross-references** -- links between markdown documents and code

The result: `search(node: "my_fn", mode: "impact")` shows the blast radius of a change, following call chains discovered by the language server.

## Pipeline Integration

LSP enrichment runs as per-language `LspConsumer` instances in the EventBus pipeline. Each consumer subscribes to `LanguageDetected` events and fires `EnrichmentComplete` when done.

### Adaptive Wait (#544)

LSP servers need time after `initialized` to index the workspace. RNA uses an adaptive strategy with no fixed timeout:

1. If the server sends `experimental/serverStatus`, RNA waits indefinitely for `quiescent=true`. This is the correct signal -- pyright on large repos may need minutes.
2. If `serverStatus` never arrives (e.g., typescript-language-server), RNA probes every 5s with a lightweight `workspace/symbol` request until the server responds successfully.
3. A 10-minute circuit breaker applies in both cases -- not a normal timeout, just a safety net for servers that never become ready.

Progress is logged every 30s so long-running indexing is observable.

### Cache-Hit Bus Routing (#547)

When the EventBus content-addressed cache has a hit for an LSP consumer (same input event payload + same consumer version), the consumer's `on_event` is not called. Instead, the cached follow-on events are replayed directly. This means unchanged code roots skip LSP enrichment entirely on incremental scans.

### Dirty-Slugs Filtering (#557)

The `RootExtracted` event carries `dirty_slugs: Option<HashSet<String>>`. The `LanguageAccumulatorConsumer` uses this to emit `LanguageDetected` events only for languages with nodes in dirty roots. LSP consumers for unchanged roots are never invoked.

### Per-file didOpen Warmup (#644)

Before requesting call hierarchy or references for a symbol, RNA sends `textDocument/didOpen` for that file once per enrichment pass. Files are marked opened only after the source read and notification succeed, so transient failures can be retried. This keeps language servers that require open documents from returning empty or stale reference results during warmup.

### Operation-aware query admission

Every LSP request is admitted through one shared query profile. The profile combines the requested operation, declaration class, language/server configuration, negotiated server capabilities, and the operation's runtime budget. Synthetic values are always rejected, and declared constants are default-denied for reference queries unless a language/server profile has cleared a measured yield, correctness, latency, and reliability threshold. Rust-analyzer is currently the only built-in declared-constant opt-in; Pyright remains disabled because the maintained probe produced only timeouts and no edges. High-signal function call hierarchy and trait implementation requests remain enabled when the server advertises those capabilities.

RNA records scheduled requests, non-empty responses, emitted edges, latency, timeouts, and errors for each language/server, operation, and declaration class. `list_roots` exposes these query-yield rows beneath each language's LSP summary so operators can compare request cost with agent-visible graph value.

The reproducible probe is:

```bash
cargo test measure_declared_const_reference_yield -- --ignored --nocapture --test-threads=1
```

It runs maintained Rust and Python fixtures sequentially through RNA's actual LSP enricher and asserts the current per-server decision.

## Auto-detected Language Servers

### Common Servers (install for richer graphs)

| Language | Server | Install |
|---|---|---|
| Rust | rust-analyzer | `rustup component add rust-analyzer` |
| Python | pyright | `npm install -g pyright` |
| TypeScript/JS | typescript-language-server | `npm install -g typescript-language-server typescript` |
| Go | gopls | `go install golang.org/x/tools/gopls@latest` |
| C/C++ | clangd | ships with LLVM / `brew install llvm` |
| Markdown | marksman | `brew install marksman` |

Plus 32 more: Ruby (solargraph), Java (jdtls), C# (omnisharp), Kotlin, Lua, Zig, Elixir, Haskell, OCaml, Scala, Dart, PHP, Swift, R, Julia, CSS, HTML, JSON, Nix, Terraform, TOML, YAML, Vue, Svelte, Erlang, Gleam, Nim, Clojure, Deno, Protobuf (buf), LaTeX (texlab), Typst (tinymist). Full list in `src/extract/consumers.rs`.

## Capability Readiness

MCP output distinguishes freshness from readiness. Freshness says when the index last changed; readiness says which workflow metadata is currently trustworthy: exact extracted graph search, embeddings/semantic search, LSP call/reference coverage, and dead-code prerequisites. Dead-code workflows require complete, persisted, non-zero LSP call/reference coverage; if LSP is still running, failed, or unavailable, the readiness block reports that instead of implying the graph is complete.

If a language server stops making progress and RNA aborts the pass, graph finalization still completes and preserves every node and edge produced before the abort. CLI and MCP readiness report this terminal result as `partial/degraded` with the original abort diagnostic. RNA does not write the full-LSP completion sentinel for a degraded run, so a later scan can retry instead of treating partial coverage as complete.

## Type Hierarchy Enrichment

When a language server advertises `typeHierarchyProvider`, RNA queries supertypes for each Trait, Struct, and Enum node to create compiler-accurate `Implements` edges (e.g., `MyStruct -> MyTrait`).

**How it works:**

1. During initialization, RNA checks the server's `capabilities.typeHierarchyProvider` field
2. Enrichment runs as a separate second pass after call hierarchy (Pass 1: calls/implementations/links, Pass 2: type hierarchy batch)
3. For each eligible node, `prepareTypeHierarchy` resolves the node, then `typeHierarchy/supertypes` discovers parent traits/interfaces
4. Results are resolved against the graph using name + file + position matching, with tiebreakers for same-named types

**Resilience:**

- A strike counter tracks consecutive failures. After 3 strikes (`MAX_TYPE_HIERARCHY_STRIKES`), type hierarchy is disabled for the rest of the enrichment pass to avoid stalling on broken servers
- Strikes reset on any successful prepare call
- Servers that don't support type hierarchy are detected at init and skipped entirely

**Concurrency:**

- LSP requests within a single language server use pipelined transport with adaptive concurrency (TCP slow-start from 4 to 64 concurrent requests). Different language servers run in parallel via separate `LspConsumer` instances in the EventBus.

### Reference Edge Coverage

LSP reference queries emit confirmed `ReferencedBy` edges from the referring symbol/location to the referenced node. Post-extraction import-call analysis also emits detected `ReferencedBy` edges for unambiguous imports and attribute-access references captured in `metadata["attr_refs"]`; those tree-sitter-derived edges complement LSP rather than replacing confirmed LSP coverage.

**Limitations:**

- Subtypes are not queried -- `find_implementations` already covers that direction for traits, and Rust structs/enums cannot have subtypes
- Non-Rust language servers (Java, TypeScript) may benefit from subtype queries in the future
