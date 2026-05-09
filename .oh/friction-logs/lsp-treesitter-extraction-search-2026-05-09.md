---
id: lsp-treesitter-extraction-search-2026-05-09
outcome: context-assembly
severity: friction
---

# LSP/tree-sitter extraction search friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-05-09 | `mcp_rna_server_search` query `LangConfig language parser tree_sitter extractor suffixes` | friction | Search returned no results, despite `src/extract/generic.rs` defining `LangConfig` and `src/extract/configs.rs` defining many `LangConfig` instances. | Investigation of extraction capabilities could miss the canonical language configuration model and over-rely on fallback source inspection. | Improve exact/compound search recall for Rust struct definitions and construction sites; verify `LangConfig` symbols and instances are indexed and retrievable. |
| 2026-05-09 | `mcp_rna_server_search` queries for parser dependency names in `Cargo.toml` | friction | Search returned no results for parser dependency names, despite `Cargo.toml` lines 24-52 listing tree-sitter grammar crates. | Agents cannot reliably discover available parser coverage through MCP search and must inspect manifest text directly. | Ensure manifest/dependency sections are searchable, especially exact crate names and `tree-sitter-*` dependencies. |
| 2026-05-09 | `mcp_rna_server_search` query `tree_sitter_` scoped to `src/extract` | friction | Search returned only a test function and missed actual parser registrations/usages in extraction configs; specialized `grep` was needed. | MCP search under-reported tree-sitter usage and could lead agents to conclude parser registration coverage is absent. | Improve underscore/prefix matching and symbol/body/config recall for parser registration calls in `src/extract`. |
