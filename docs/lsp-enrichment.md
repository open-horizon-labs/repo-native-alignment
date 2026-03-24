# LSP Enrichment

RNA auto-discovers installed language servers and enriches the graph with cross-file edges. No configuration -- if the binary is on PATH, it's used. Missing servers are skipped gracefully.

## What LSP Adds Beyond tree-sitter

- **Who calls this?** -- inbound call graph, not just the function definition
- **What does this call?** -- outbound call chain, including into external packages (`tokio`, `lancedb`, your dependencies)
- **Who implements this trait/interface?** -- implementation edges across files
- **Doc cross-references** -- links between markdown documents and code

The result: `graph_query(mode: "impact", from: "my_fn")` shows the blast radius of a change, following call chains discovered by the language server.

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

## 38 Auto-detected Language Servers

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

**Limitations:**

- Subtypes are not queried -- `find_implementations` already covers that direction for traits, and Rust structs/enums cannot have subtypes
- Non-Rust language servers (Java, TypeScript) may benefit from subtype queries in the future
