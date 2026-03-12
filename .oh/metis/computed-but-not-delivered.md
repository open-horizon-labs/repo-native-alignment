---
id: computed-but-not-delivered
outcome: agent-alignment
title: New metadata fields must be wired through 3 layers — extraction alone is not delivery
---

## What Happened

PR #137 added cyclomatic complexity scoring. The extraction layer computed the value correctly (8 unit tests passing across 5 languages), the /ship pipeline marked it "manual verified", and it was pushed to the PR. But the value was invisible to agents — `search_symbols`, `graph_query`, and `oh_search_context` all returned results without complexity scores.

Two layers were missing:
1. **LanceDB persistence** — metadata fields are stored as typed Arrow columns, not a generic map. Without a `cyclomatic` Int32 column in the schema, the value was lost on every persist/reload cycle.
2. **MCP tool rendering** — even with persistence, the output formatting in `server.rs` cherry-picks which metadata to display. Three separate render sites needed updating.

## The Pattern

> Computing a value is not delivering it. New node metadata must be wired through all 3 layers: extraction → LanceDB schema+columns → MCP tool output rendering.

The delivery pipeline's "manual verification" step tried to use RNA tools to verify, but interpreted "no results" as "index hasn't rebuilt yet" rather than "the data isn't persisted." The test suite validated computation but not the full data path.

## How to Apply

When adding new metadata to `Node`:
1. Add the field extraction in the extractor (tree-sitter pass)
2. Add a typed Arrow column in `symbols_schema()` in `store.rs` + bump `SCHEMA_VERSION`
3. Wire the write path (2 sites in `server.rs`: initial batch + upsert batch)
4. Wire the read path (1 site in `server.rs`: Arrow → Node reconstruction)
5. Add rendering in all MCP tool output formatters (`search_symbols`, `graph_query`/`format_neighbor_nodes`, `oh_search_context`)
6. **Verify end-to-end**: after install, restart, and rescan — confirm the value appears in actual tool output

## Evidence Source

PR #137 (cyclomatic complexity), discovered during live testing with user.
