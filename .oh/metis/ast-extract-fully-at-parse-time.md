---
id: ast-extract-fully-at-parse-time
outcome: agent-alignment
title: Extract all AST-available data at parse time — the AST is only available once
---

## What Happened

PR #44 fixed a fragile LSP cursor column detection bug by storing `name_col` in Node.metadata at parse time (during tree-sitter extraction). Previously, the code tried to re-derive the column at LSP enrichment time, which was brittle and wrong.

The fix required recognizing that the tree-sitter AST is available exactly once — during extraction. At enrichment time, only the file contents and the graph exist.

## The Pattern

> Capture everything the AST can tell you during the extraction pass. Store it in Node.metadata.

This includes:
- Name span (start row, start col, end col)
- Visibility modifiers (pub, pub(crate), etc.)
- Generic parameters
- Documentation comments (which appear as sibling nodes in the AST)
- Return type spans

Trying to recover this information later (from file contents + heuristics) is fragile and slower.

## Generalization

This pattern applies to any multi-pass architecture where a structured representation (AST, parse tree, IR) is only available in the first pass:
- Don't defer extraction to save memory — the cost of a second pass or a heuristic re-derivation is higher
- Store position information in absolute terms (line, col) not relative terms, since the file may change

## Evidence Source

PR #44 (fix fragile LSP cursor column detection), `name_col` stored in Node.metadata pattern.
