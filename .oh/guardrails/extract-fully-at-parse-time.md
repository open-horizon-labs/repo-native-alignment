---
id: extract-fully-at-parse-time
outcome: agent-alignment
severity: candidate
statement: Capture all AST-available metadata during the extraction pass. The AST is only available once. Don't re-derive position, visibility, or structural information from file contents at enrichment time.
---

## Rationale

The tree-sitter AST is constructed during the extraction pass and discarded after. Enrichers (LSP, metadata patchers) operate on the graph — they see node IDs and file paths, not the original AST. Any structural information not captured during extraction must be re-derived from file text, which is fragile and slower.

## Evidence

PR #44 (fix fragile LSP cursor column detection): LSP cursor placement used a heuristic to find `name_col` from file text. This was fragile — the heuristic misidentified column positions for some function signatures. Fix: store `name_col` in Node.metadata at tree-sitter parse time. One-line fix that eliminated an entire class of LSP enrichment failures.

Source: ast-extract-fully-at-parse-time.md

## What to Capture

At extraction time, store in Node.metadata:
- Name span: start_row, start_col, end_col (for LSP cursor placement)
- Visibility modifiers (pub, pub(crate), private)
- Generic parameter count
- Doc comment text (appears as adjacent AST nodes)

## Override Protocol

Skip capturing fields that require semantic analysis (type resolution, cross-file references) — those legitimately belong to the enrichment phase. The guardrail applies to syntactic/positional information available from the parse tree.
