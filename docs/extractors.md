# Extractors

RNA includes 22 language extractors that run via tree-sitter to produce symbols, import graphs, and topology edges.

## Supported Languages

- **Code** -- Rust, Python, TypeScript/TSX, JavaScript/JSX, Go, Java, Bash, Ruby, C++, C#, Kotlin, Zig, Lua, Swift
- **Config & infra** -- HCL/Terraform, JSON, TOML, YAML (Kubernetes manifests detected automatically)
- **Docs & schema** -- Markdown (heading-aware), .proto, SQL, OpenAPI
- **Architecture** -- subprocess, network, async boundaries detected as topology edges (Rust extractor)

## Constants and Literals (cross-language)

All 22 extractors index constants and literal values. `search_symbols` returns the value inline:

```
- const MAX_RETRIES (rust) src/config.rs:12  Value: `5`
- const MAX_RETRIES (python) settings.py:3   Value: `5`
- const MAX_RETRIES (go) config.go:8         Value: `5`
```

Named constants are declared identifiers -- `const MAX_RETRIES = 5`, static final fields, ALL_CAPS module-level assignments, etc.

Synthetic constants are inferred from structure -- YAML/TOML/JSON top-level scalar values, OpenAPI enum values, and single-token string literals (e.g. `"application/json"`, `"GET"`) found in function bodies. They appear with a `*(literal)*` badge.

`search_symbols` accepts a `synthetic` filter to narrow results to declared constants, inferred literals, or both.

## Language Mapping

- **Rust** -- `const_item` with extracted value
- **Python** -- module-level ALL_CAPS assignments (`[A-Z][A-Z0-9_]+`)
- **TypeScript/JavaScript** -- module-level `const` declarations
- **Go** -- `const_spec` inside `const_declaration`
- **Java** -- `static final` field declarations
- **Kotlin** -- `const val` property declarations
- **C#** -- `const` field declarations
- **Swift** -- module-level `let` bindings
- **Zig** -- `const` variable declarations
- **C/C++** -- `constexpr` and `static const` declarations
- **Lua/Ruby/Bash** -- ALL_CAPS module-level assignments
- **HCL** -- `variable` block default values
- **Proto** -- enum values and `option` fields
- **SQL** -- `CREATE TYPE ... AS ENUM` values
- **YAML/TOML/JSON/OpenAPI** -- top-level scalar values (synthetic)

## Embeddings

Local Metal GPU via metal-candle (CPU fallback), no API key needed.
