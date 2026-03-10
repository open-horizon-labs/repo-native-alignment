# LSP Enrichment

RNA auto-discovers installed language servers and enriches the graph with cross-file edges. No configuration -- if the binary is on PATH, it's used. Missing servers are skipped gracefully.

## What LSP Adds Beyond tree-sitter

- **Who calls this?** -- inbound call graph, not just the function definition
- **What does this call?** -- outbound call chain, including into external packages (`tokio`, `lancedb`, your dependencies)
- **Who implements this trait/interface?** -- implementation edges across files
- **Doc cross-references** -- links between markdown documents and code

The result: `graph_query(mode: "impact", from: "my_fn")` shows the blast radius of a change, following call chains discovered by the language server.

## 37 Auto-detected Language Servers

### Common Servers (install for richer graphs)

| Language | Server | Install |
|---|---|---|
| Rust | rust-analyzer | `rustup component add rust-analyzer` |
| Python | pyright | `npm install -g pyright` |
| TypeScript/JS | typescript-language-server | `npm install -g typescript-language-server typescript` |
| Go | gopls | `go install golang.org/x/tools/gopls@latest` |
| C/C++ | clangd | ships with LLVM / `brew install llvm` |
| Markdown | marksman | `brew install marksman` |

Plus 31 more: Ruby (solargraph), Java (jdtls), Kotlin, Lua, Zig, Elixir, Haskell, OCaml, Scala, Dart, PHP, Swift, Nix, Terraform, TOML, YAML, and others. Full list in `src/extract/mod.rs`.

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

**Limitations:**

- Concurrent LSP requests are not yet supported (transport uses `&mut self`). Requests are sequential, which is acceptable at RNA's current scale (~70 requests x <10ms each)
- Subtypes are not queried -- `find_implementations` already covers that direction for traits, and Rust structs/enums cannot have subtypes
- Non-Rust language servers (Java, TypeScript) may benefit from subtype queries in the future
