---
id: computed-but-not-delivered
outcome: context-assembly
severity: hard
statement: New metadata must wire through 3 layers — extraction, LanceDB schema, MCP rendering. Computing a value is not delivering it.
---

## The Pattern

When adding new metadata to `Node`:
1. Add field extraction in the extractor (tree-sitter pass)
2. Add typed Arrow column in `symbols_schema()` in `store.rs` + bump `SCHEMA_VERSION`
3. Wire write path (initial batch + upsert batch in `server.rs`)
4. Wire read path (Arrow → Node reconstruction in `server.rs`)
5. Add rendering in MCP tool output formatters
6. **Verify end-to-end**: after install, restart, rescan — confirm the value appears in actual tool output

## Override Protocol

None. If a metadata field is worth computing, it's worth delivering. Skip only if the field is intentionally internal (not surfaced to agents).

## Evidence

PR #137 (cyclomatic complexity): extraction worked, tests passed, /ship marked "manual verified" — but the value was invisible to agents. Two layers missing: LanceDB schema column and MCP output rendering.

Source: metis/computed-but-not-delivered.md
