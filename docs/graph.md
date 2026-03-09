# Graph Architecture

RNA builds and serves a multi-language code graph using LanceDB for storage and petgraph for traversal.

## Structure

```
Scanner (mtime + git)     <- incremental file change detection
  ├── tree-sitter         <- Rust, Python, TS, Go -> symbols + import graph
  ├── schema extractors   <- .proto, SQL, OpenAPI -> schema nodes + edges
  ├── topology detector   <- subprocess/network/async boundaries -> architecture edges
  ├── markdown extractor  <- heading sections + YAML frontmatter -> graph nodes
  ├── LSP enricher        <- 37 servers auto-detected -> calls, implements, doc links
  └── metal-candle        <- local GPU embeddings (Metal-accelerated, CPU fallback)
         |
         v
Graph (LanceDB + petgraph)
  ├── LanceDB             <- columnar + vector store
  ├── petgraph            <- in-memory traversal (BFS, impact, reachability)
  └── SourceEnvelope      <- canonical records with scope + provenance
         |
         v
MCP Server (rust-mcp-sdk) <- stdio + HTTP transport, 9 tools
```

## Nodes and Edges

- **Nodes:** symbols, schemas, artifacts, PR merges
- **Edges:** calls, implements, depends-on, modified, serves (with provenance + confidence)
- **Traversal:** in-memory via petgraph (microseconds)

No cloud dependency. Everything local, git-versioned, disposable.
