# Session: relevance-scoring

## Aim
Agents get the most outcome-relevant results first from RNA search tools, so they can make decisions without manually sifting through noise.

## Problem Space
**Updated:** 2026-03-10

### Objective
Optimize for: agents get the most outcome-relevant results first.

### Current State

| Search path | Scoring today | Gap |
|------------|--------------|-----|
| `oh_search_context` artifacts | Vector similarity (L2→score) | Good |
| `oh_search_context` code symbols | Substring match, no ranking | **Major** |
| `oh_search_context` markdown | Substring match, no ranking | **Major** |
| `oh_search_context` cross-source | Concatenated, no interleaving | **Major** |
| `search_symbols` | 5-tier cascade (exact > contains > sig, kind rank, test penalty, edge count) | Good |
| `graph_query` | BFS order, no scoring | Moderate |
| `outcome_progress` commits | Tagged-first, then unranked | Moderate |
| `outcome_progress` symbols | File iteration order | Moderate |

Key files:
- `src/server.rs:1983-2101` — oh_search_context handler
- `src/server.rs:2150-2318` — search_symbols handler (5-tier ranking)
- `src/server.rs:2320-2473` — graph_query handler
- `src/query.rs:23-101` — outcome_progress
- `src/embed.rs:561-653` — EmbeddingIndex.search() (L2→similarity score)

### Constraints

| Constraint | Type | Reason | Question? |
|-----------|------|--------|-----------|
| No external service dependencies | Hard | RNA runs fully local (Metal GPU embeddings, no API keys) | No |
| MiniLM-L6-v2 embedding model | Soft | Deployed, Metal GPU optimized | Could swap for code-specific model |
| Results returned as markdown text | Soft | MCP tool response format | Could return structured JSON |
| LanceDB is the vector store | Hard | Architectural choice, embedded | No |
| Sub-second response time | Hard | Agent workflow can't block | No |

### Terrain

**What other systems do:**
- CGR: multi-source fusion with explicit scores (function 0.9, class 0.8, variable 0.7, content 0.6), dependency penalty (-0.2)
- CGC: single-source Cypher, results ordered by graph DB
- OpenDev: no ranking — ephemeral LSP results, agent reasons over them

**Underutilized signals already in RNA:**
- Embedding scores exist for artifacts but code/markdown searched by substring only
- Edge count (connectivity) computed in search_symbols but not used as ranking signal elsewhere
- search_symbols 5-tier cascade is a good pattern — could extend to other paths
- Outcome file patterns could boost symbols in outcome-linked files

### Assumptions
1. Agents consume results top-to-bottom — ranking matters
2. "Relevance" = closest to query intent (not task-specific)
3. Unified cross-source score is possible and desirable

### X-Y Check
- **Stated (Y):** Add relevance scoring
- **Underlying (X):** Agents get useful context without extra filtering
- **Note:** Semantic graph entry points (querying graph by intent, not node ID) may matter more than ranking within results. If agents can't find the graph, ranking within it is moot.

### Key Question for Solution Space
Rank within each source independently? Or build unified cross-source ranking that interleaves artifacts, code, markdown, and commits by a comparable score?

## Execute
**Updated:** 2026-03-10
**Status:** complete
**PR:** https://github.com/open-horizon-labs/repo-native-alignment/pull/112
**Branch:** feat-relevance-scoring

### Approach chosen: A (within-source ranking)

Implemented within-source ranking for the two Major gaps in `oh_search_context`:

1. **Code symbols** — reused the 5-tier cascade from `search_symbols` (exact name > name contains > signature-only, kind rank, test penalty, edge count). Changed from `.take(limit)` before sorting to sort-then-truncate so limit returns top-N most relevant.

2. **Markdown chunks** — new `search_chunks_ranked()` function scoring by: heading match quality (exact 1.0, contains 0.7, content-only 0.4), heading level bonus (h1: +0.10, h2: +0.08, h3: +0.06), code span cross-reference bonus (+0.15), match density (capped at +0.10).

### Changes
- `src/markdown/mod.rs`: Added `ScoredMarkdownChunk` struct and `search_chunks_ranked()` function with 4-tier scoring. 5 new tests.
- `src/server.rs`: Updated `oh_search_context` code symbols section to use 5-tier ranking (sort-then-truncate). Updated markdown section to use `search_chunks_ranked` with scores in output.

### Not done (deferred)
- Cross-source interleaving (approach B) — would need score normalization across fundamentally different scoring domains
- `graph_query` ranking — moderate gap, separate concern
- `outcome_progress` ranking — moderate gap, separate concern

### Test results
199 tests pass (194 existing + 5 new markdown ranking tests)
