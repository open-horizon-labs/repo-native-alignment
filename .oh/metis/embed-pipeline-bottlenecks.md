---
id: embed-pipeline-bottlenecks
outcome: agent-alignment
title: Embedding pipeline had 4 compounding perf bottlenecks
---

On large repos (oh-omp: 3,406 commits, 1,421 files), the embedding pipeline embedded 99K+ items because:

1. `load_commits(usize::MAX)` walked and diffed every commit in history
2. String literals created one node per occurrence, not per unique value
3. `TextEmbedding` ONNX model was re-initialized per operation (index, reindex, search)
4. No content-hash cache — table drops forced full re-embed

Fix: cap commits (500+250 merges), dedup strings by value, share model in struct, BLAKE3 embed cache.

oh-omp went from ~7,380 items to ~2,991 on cold start; near-zero on warm runs.

Key insight: the model itself was fast (BGE-small). The problem was feeding it 20x more data than needed. Measure what you embed, not how fast you embed it.
