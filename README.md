# repo-native-alignment

An agentic harness where business-specific outcomes, SLO signals, constraints, and learnings live in the repo as queryable artifacts — so AI coding agents stay aligned to declared intent, not just code correctness.

## What this is

A system that gives agents structured answers to:
- "What business outcomes are we optimizing for?"
- "What signals tell us we're on track?"
- "What constraints must I respect?"
- "What have we learned from past work?"

These answers live in `.oh/` as structured markdown, versioned by git, queryable via MCP tools.

## Architecture

```
git2              <- change detection (what changed since last indexed?)
tree-sitter       <- parse code -> symbols
pulldown-cmark    <- parse markdown -> sections (including .oh/ business context)
LanceDB           <- store + search (columnar + vectors + full-text, embedded)
DuckDB (optional) <- SQL analytics overlay via lance-duckdb extension
MCP server        <- agent interface
```

## Status

Bootstrapping. See `.oh/repo-native-alignment.md` for the aim and exploration history.
