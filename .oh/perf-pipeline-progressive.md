---
title: "Performance: Pipeline Progressive Availability"
aim: agent-alignment
created: 2026-03-09
---

## Aim

Reduce time from MCP server start to first useful agent query response.
Currently 8 minutes on oh-omp. Should be seconds.

## Problem Space
**Updated:** 2026-03-09

### Objective
Time-to-queryable. Agent gets useful answers in seconds, not minutes.

### What shipped (PRs #69, #76)
- NodeKind::is_embeddable() filter: 99K -> 26K items (73% reduction)
- Commit cap: usize::MAX -> 100 recent + 50 PR merges
- Batched embedding with progress (100/batch)
- FTS index on symbols.name for non-embedded nodes
- LSP moved after persist+embed (graph persisted at 3.8s)
- `scan` CLI subcommand
- Simplified text assembly (signature-only for code)

### What's still blocking (oh-omp timings)

| Phase | Time | Blocks query? | Must block? |
|-------|------|---------------|-------------|
| Scan + extract | 3.4s | Yes | Yes |
| Persist to LanceDB | 0.4s | Yes | Yes |
| **Embed 26K texts (CPU)** | **129s** | No | **No** |
| **rust-analyzer** | **336s** | No | **No** |
| TS LSP | 1.7s | No | No |

474 of 483 seconds are spent on things that don't need to block.

### Constraints

| Constraint | Type | Reason | Questionable? |
|------------|------|--------|---------------|
| Local-first embedding | hard (guardrail) | No API key for core | Progressive: local fast, API better |
| ONNX/CPU inference ~200 texts/sec | **FALSE** | Default config | CoreML on Apple Silicon: 10-50x via ANE |
| LSP servers are blocking | assumed | Implementation choice | LSP protocol is async |
| All enrichment before graph available | assumed | build_full_graph returns late | Could return at persist and spawn rest |

### Key discovery: CoreML execution provider

fastembed's `ort` crate has `CoreML` EP built in. Apple Neural Engine is designed for exactly this workload (small transformer inference). Enabling it:

```toml
# Cargo.toml
ort = { version = "2", features = ["coreml"] }
```

```rust
InitOptions::new(EmbeddingModel::BGESmallENV15)
    .with_execution_providers(vec![
        ort::execution_providers::CoreMLExecutionProvider::default().build()
    ])
```

Expected: 129s -> 3-13s on M-class Mac.

### Solution tracks

1. **Return graph at persist point (~4s)** - spawn embed + LSP as background tasks
2. **CoreML acceleration** - enable ANE for embedding, 10-50x speedup
3. **BLAKE3 embed cache** - content-addressed vectors survive rebuilds (warm start = free)
4. **LSP parallelism** - run language servers concurrently
</content>
</invoke>