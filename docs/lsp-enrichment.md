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
