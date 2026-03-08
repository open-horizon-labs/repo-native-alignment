# Session: constants-cross-language

## Aim
Agents working across any file in the repo can see related constants and literal values in other languages/systems when they search — making silent divergence less likely without adding new workflows.

## Problem Space
**Updated:** 2026-03-08

**Objective:** When an agent looks up a constant, show it the same constant everywhere it lives across the repo.

**Terrain:**
- `SymbolKind::Const` exists in `src/types.rs:101` — defined, serialized
- `const_item` → `Const` only in `src/extract/rust.rs:73` — one of 22 extractors
- `body` stores full raw text — scalar values are in there, just not parsed out
- 22 extractors covering Python, TS, Go, Java, YAML, TOML, JSON, HCL, proto — none emit `Const`

**Constraints:**
- Hard: repo-native, lightweight, no new MCP tools
- Guardrail: capture all AST-available metadata during extraction (not at enrichment time)
- Soft guardrail: LLMs surface candidates; humans judge

## Solution Space
**Updated:** 2026-03-08

**Selected:** Option C — extend 22 extractors + `value: Option<String>` + `synthetic: bool`
**Level:** Local Optimum (deliberate — architecture is right, capture is missing)

**Schema change** (`src/types.rs`, `CodeSymbol`):
```rust
pub value: Option<String>,   // scalar value if extractable ("5", "application/json")
pub synthetic: bool,          // true = inferred literal, false = declared constant
```

**Per-language "constant" constructs:**
| Language | Construct | synthetic |
|---|---|---|
| Rust | `const_item` (already) | false |
| Python | module-level ALL_CAPS assignment | false |
| TypeScript/JS | module-level `const`, `readonly` property | false |
| Go | `const` spec | false |
| Java | `static final` field | false |
| Kotlin | `const val`, companion object `const` | false |
| C# | `const` field | false |
| Swift | `let` at module level | false |
| Zig | `const` declaration | false |
| C/C++ | `constexpr`, `static const` | false |
| Lua | module-level ALL_CAPS assignment | false |
| Ruby | ALL_CAPS constant | false |
| Bash | exported uppercase vars | false |
| YAML/TOML/JSON | top-level scalar key-value | true |
| HCL | `variable` defaults, `locals` | false |
| Proto | enum values, `option` fields | false |
| SQL | enum types, check constraint values | false |
| String literals (all, len > 3) | inline in function bodies | true |

**Display** (`CodeSymbol::to_markdown`):
```
- `src/config.rs:12` **const** `MAX_RETRIES = 5`
- `src/handler.rs:44` **const** `"application/json"` *(literal)*
```

**Accepted trade-offs:**
- Python ALL_CAPS heuristic may have false positives
- YAML/TOML/JSON top-level scalars are `synthetic: true` (correct — no language-level const)
- Derived constants (`const T: u64 = BASE * 2`) leave `value: None`

## Acceptance Criteria

- [x] `CodeSymbol` has `value: Option<String>` and `synthetic: bool` fields, serialized correctly
- [x] All 22 extractors emit `SymbolKind::Const` for language-appropriate constructs with `value` extracted where available
- [x] Rust extractor: value extracted from `value` child node (not just body text)
- [x] Python: module-level ALL_CAPS assignments captured as `synthetic: false`
- [x] TypeScript/JS: module-level `const` declarations captured
- [x] Go: `const` spec captured
- [x] YAML/TOML/JSON: top-level scalar key-values captured as `synthetic: true`
- [x] HCL: `variable` defaults captured
- [x] Proto: enum values and `option` fields captured
- [ ] Single-token string literals (len > 3) captured as `synthetic: true` in all code extractors (deferred — high noise, separate issue)
- [x] `CodeSymbol::to_markdown` displays value inline; synthetics badged with `*(literal)*`
- [x] `search_symbols` MCP tool returns cross-language constants with value visible in results
- [x] README documents the constants/literals capture behavior
- [x] Smoke test covers: `const_extraction` check verifies Const nodes with values
- [x] No new MCP tools added; `search_symbols` remains the sole surface

## Execute
**Updated:** 2026-03-08
**Status:** complete

Implemented on branch `feat/cross-language-constants`, PR #50.

All 22 extractors updated to emit `NodeKind::Const` with `value` and `synthetic` in `graph::Node.metadata`.

Key decisions:
- Used `metadata: BTreeMap<String, String>` (existing field) instead of new struct fields — no schema migration needed
- String literal capture (spec item 4) deferred: extremely high noise in practice, should be a separate opt-in feature
- SQL enum support uses `Statement::CreateType` (PostgreSQL) via sqlparser-rs; generic `CHECK` constraint parsing deferred
- HCL `locals` block entries not captured (non-trivial AST — `locals` is not a block with named children in tree-sitter-hcl); `variable` defaults work

Test count: 155 → 159 (4 new tests added for Rust, Python, Go, YAML const extraction).

## Review
**Updated:** 2026-03-08
**Status:** blocking issue found, comment posted to PR #50

### Blocker
`value` and `synthetic` are stored in `graph::Node.metadata` (in-memory), but `symbols_schema` (`src/graph/store.rs:15`) has no `metadata` column. On server restart, `build_full_graph` short-circuits to LanceDB when no file changes are detected (`src/server.rs:1078-1092`). Nodes loaded from LanceDB have `metadata: BTreeMap::new()` (`src/server.rs:607`). The `search_symbols` display at `src/server.rs:1886-1891` finds nothing — `Value:` lines are silently absent after restart.

Fix: add `value` and `synthetic` columns to `symbols_schema` and populate/restore them in the persist/load paths.

### Non-blocking findings
- HCL variables without `default` value still emit a Const node (no value) — should guard on `default_val.is_some()`
- Go multi-name `const A, B = 1, 2` — only `A` captured (`child_by_field_name("name")` returns first identifier)
- Smoke test exercises in-memory extraction path only, not the LanceDB roundtrip
- No tracking issue for deferred string literal capture

## String Literal Capture Execute
**Updated:** 2026-03-08
**Status:** complete

Implemented on branch `feat/string-literal-constants`, PR #57 (stacked on `feat/cross-language-constants`).

Shared harvester in `src/extract/string_literals.rs`. All 14 code extractors call `harvest_string_literals()`.

Key decisions:
- Separate module file (`string_literals.rs`) avoids auto-formatter stripping inline additions to `mod.rs`
- Per-language string node kinds: Rust/Kotlin/C++/C# use named child (`string_content`/`string_literal_segment`); TypeScript/JS use `string_fragment`; Go/Java/Zig/Lua/Bash strip raw quotes; Ruby targets `string_content` nodes directly
- Filter: len > 3 keeps meaningful values; dedup by `(value, line_start)` within file

Tests: 159 -> 166 (7 new tests covering shared helper, cross-language, and filter behavior).
