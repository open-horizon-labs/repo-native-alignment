# Graph Architecture

RNA builds and serves a multi-language code graph using LanceDB for storage and petgraph for traversal.

## Structure

The extraction pipeline is fully event-driven via an `EventBus` with registered consumers. Each consumer reacts to events and may emit follow-on events. The bus drains depth-first until no events remain.

```
Scanner (mtime + git)          <- incremental file change detection
         |
         v
EventBus.emit_all(RootDiscovered)
         |
         v
ManifestConsumer               <- package.json/Cargo.toml dependency edges
TreeSitterConsumer             <- 30 extractors (22 code + 4 config + 4 schema)
  ├── rayon parallel per-file extraction
  ├── topology detector        <- subprocess/network/async boundaries
  └── fires: RootExtracted { nodes, edges, dirty_slugs }
         |
         v
LanguageAccumulatorConsumer    <- groups nodes by language
  └── fires: LanguageDetected (once per language)
         |
         v
LspConsumer × N                <- 38 servers auto-detected, one consumer per language
  └── fires: EnrichmentComplete { added_edges, new_nodes }
         |
AllEnrichmentsGate             <- counts expected vs received, fires AllEnrichmentsDone
         |
         v
EnrichmentFinalizer            <- runs post-extraction passes over full graph
  ├── FrameworkDetectionConsumer <- detects frameworks from imports
  ├── FastapiRouterPrefixConsumer
  ├── SdkPathInferenceConsumer
  ├── NextjsRoutingConsumer    <- monorepo-aware route extraction
  ├── PubSubConsumer           <- Kafka, Celery, Pika, Redis pub/sub edges
  ├── WebSocketConsumer        <- Socket.IO, ws edges
  └── fires: PassesComplete { nodes, edges, detected_frameworks }
         |
         v
SubsystemConsumer              <- Louvain community detection, PageRank
LanceDBConsumer                <- background persist (full or incremental)
EmbeddingIndexerConsumer       <- streams embed tasks in parallel with LSP
ScanStatsConsumer              <- live stats for list_roots (no file I/O)
         |
         v
Graph (LanceDB + petgraph)
  ├── LanceDB                  <- columnar + vector store
  ├── petgraph                 <- in-memory traversal (BFS, impact, reachability)
  └── content-addressed cache  <- per-consumer cache keys, dirty-slugs filtering
         |
         v
MCP Server (rust-mcp-sdk)      <- stdio + HTTP transport, 4 tools
```

## Nodes and Edges

- **Nodes:** symbols, schemas, artifacts, PR merges, framework nodes, channel nodes, subsystem metadata
- **Edges:** calls, implements, depends-on, modified, serves, produces, consumes, uses-framework (with provenance + confidence)
- **Traversal:** in-memory via petgraph (microseconds)

No cloud dependency. Everything local, git-versioned, disposable.
