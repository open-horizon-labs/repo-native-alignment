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
TreeSitterConsumer             <- built-in code/config/schema/doc extractors
  ├── rayon parallel per-file extraction
  ├── topology detector        <- subprocess/network/async boundaries
  └── fires: RootExtracted { nodes, edges, dirty_slugs }
         |
         +--> OpenApiConsumer  <- bidirectional endpoint / SDK operation linking
         +--> GrpcConsumer     <- proto RPC -> caller stub matching
         +--> EmbeddingIndexerConsumer <- streams embed tasks in parallel with LSP
         |
         v
LanguageAccumulatorConsumer    <- groups nodes by language
  └── fires: LanguageDetected (once per language)
         |
         v
LspConsumer × N                <- auto-detected servers, one consumer per language
  └── fires: EnrichmentComplete { added_edges, new_nodes, updated_nodes }
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
ScanStatsConsumer              <- live stats for list_roots (no file I/O)
OperationReportStore           <- durable operation telemetry for scans/enrichment
         |
         v
Graph (LanceDB + petgraph)
  ├── LanceDB                  <- columnar + vector store
  ├── petgraph                 <- in-memory traversal (BFS, impact, reachability)
  └── content-addressed cache  <- per-consumer cache keys, dirty-slugs filtering
         |
         v
MCP Server (rust-mcp-sdk)      <- stdio + HTTP transport
```

## Nodes and Edges

- **Nodes:** symbols, schemas, artifacts, PR merges, framework nodes, channel nodes, subsystem metadata
- **Edges:** calls, implements, depends-on, modified, serves, produces, consumes, uses-framework, referenced-by (with provenance + confidence)
- **Traversal:** in-memory via petgraph (microseconds)
- **Readiness:** MCP responses surface exact-search, embedding, LSP call/reference, and dead-code prerequisite readiness separately from index freshness.

No cloud dependency. Everything local, git-versioned, disposable.
