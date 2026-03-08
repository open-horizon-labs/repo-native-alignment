---
id: targeted-reindex-better-than-full-reindex
outcome: agent-alignment
title: reindex_nodes() for targeted re-embed is better than full reindex when metadata changes
---

## What Happened

PR #41 (LSP metadata patches now re-trigger embeddings) introduced `reindex_nodes()` — a targeted re-embedding function that only re-embeds nodes whose metadata has changed, rather than re-indexing the entire graph.

The trigger: LSP enrichment patches metadata fields (type annotations, doc comments, return types) onto existing nodes. Without targeted re-embed, the embedding vectors for those nodes wouldn't reflect the enriched metadata.

## The Pattern

When a subsystem can update a node's content after initial indexing, the embedding pipeline needs a targeted invalidation path — not just a full-rebuild escape hatch. Full rebuild is O(N) nodes; targeted re-embed is O(changed nodes).

`reindex_nodes(node_ids: &[NodeId])` takes a list of affected node IDs and re-embeds only those. The implementation:
1. Fetch current node state from graph
2. Re-generate embedding text (including new metadata fields)
3. Update vector in LanceDB for those specific IDs

## Why Metadata Must Be in Embedding Text

Embedding text that doesn't include metadata fields means semantic search on type signatures, doc comments, or annotations won't find the enriched nodes. LSP enrichment is only useful for search if the embeddings reflect it.

## Evidence Source

PR #41, `reindex_nodes()` implementation, LSP metadata embedding trigger.
