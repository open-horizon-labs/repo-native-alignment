---
title: PR 780 semantic qualification friction
artifact_type: friction-log
date: 2026-07-16
outcomes:
  - context-assembly
---

# PR 780 semantic qualification friction

| Time | Severity | Event | Response |
|---|---|---|---|
| 2026-07-16 | skipped | RNA CLI located CLI search, embedding, rerank, and workflow symbols but does not deliver the implementation bodies or complete workflow YAML needed for safe contract changes. | Used bounded source reads only for the located `src/main.rs`, search diagnostics/service types, embedding/rerank model declarations, Cargo feature/profile declarations, and relevant workflow files. |
| 2026-07-16 | degraded | Workspace RNA has no embeddings and partial/degraded TypeScript LSP coverage. | Used exact structural navigation only and will not claim complete impact coverage. |
