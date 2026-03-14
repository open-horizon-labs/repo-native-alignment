---
id: perf-pipeline-session-results
outcome: agent-alignment
title: 'Embedding pipeline optimization: 99K→26K items, 8min→4s time-to-query, Metal GPU 1.8x'
---

Session 2026-03-08/09 results:

Shipped PRs: #69 (Drazen), #76, #79, #80

Volume: 99K → 26K items (is_embeddable filter, commit cap 100+50)
Time-to-query: 8min → 4s (background embed+LSP via tokio::spawn)
Embedding speed: 129s CPU → 73s Metal GPU (metal-candle MiniLM-L6-v2)
FTS: tantivy index on symbols.name covers 75K non-embedded nodes

Key learnings:
- CoreML EP doesn't work for BERT (ops fall back to CPU with 5x memory)
- metal-candle wrapper was broken (safetensors version mismatch) — fixed in our fork
- candle Metal DOES work when candle-nn has `metal` feature enabled (layer-norm kernel exists)
- Metal GPU uses unified memory — batch size must be memory-adaptive (20% cap)
- ONNX Runtime is faster than candle on CPU (568 vs 87 t/s) but Metal GPU wins (780 t/s benchmark, 354 t/s real)
- The biggest win was architectural (background tasks), not algorithmic (GPU speed)
