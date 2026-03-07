---
id: fspulse-salvage
outcome: agent-alignment
title: 'fsPulse fork salvage: scanner + extraction patterns for RNA indexing substrate'
---

## What Happened

Forked fsPulse (filesystem scanner) because the storage backend was appalling: SQLite with an event record per file per scan. Rebuilt with reverse delta storage, parent-relative paths, mtime-based subtree skipping, batch writes. Result: <10% disk, 100x+ faster scans.

## Key Learnings

1. **Reverse delta is the right model for temporal state.** Current state on the hot row, history appended. Eliminated expensive MAX() aggregations across 33+ query sites.

2. **Parent references beat full paths.** `parent_item_id + item_name` vs full `item_path` — massive storage savings, paths reconstructed via recursive CTEs.

3. **mtime-based subtree skipping is the killer optimization.** 1.5M files rescans in seconds when most subtrees unchanged. Essential for non-git directories.

4. **DirCache (bulk SELECT per directory) beats individual queries.** Original did 1.5M individual SELECTs. Grouping by directory = orders of magnitude faster.

5. **4-phase scanner state machine is overbuilt for RNA.** Scanning → Sweeping → AnalyzingFiles → AnalyzingScan solves filesystem integrity, not context indexing. RNA needs: detect changed → extract → index.

6. **Code is disposable; design patterns compound.** Scanner rewritten in a weekend. The patterns (reverse delta, mtime skip, batch writes) are the real asset.

7. **LSP gives semantics tree-sitter can't.** Cross-file references, type resolution, diagnostics — these require language servers, not just syntax parsing. Both should be extractors.

## What Transfers to RNA

- Multi-root scanning model (fsPulse's `roots` table → sharded LanceDB)
- mtime-based change detection for non-git directories
- git-aware mode as optimization tier when `.git` present
- Pluggable extractor pattern (fsPulse's `validate/` → extractors for AST, LSP, embeddings, metadata)
- Batch write patterns for LanceDB population
- Default exclude patterns per root type

## What Does NOT Transfer

- Web UI, React frontend, Axum server, WebSocket progress
- Filesystem integrity features (hash verification, FLAC/PNG/PDF validation, alerts)
- Scan scheduling (RNA is JIT, not cron)
- SQLite schema (RNA uses LanceDB)

