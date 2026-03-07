---
id: extractors-are-pluggable
outcome: agent-alignment
severity: soft
statement: Extractors (tree-sitter, LSP, embeddings, metadata) are pluggable per file type. Don't hardcode a single extraction strategy.
---

## Rationale

Different file types need different extraction: code needs AST + LSP semantics, markdown needs heading-aware chunking + embeddings, PDFs need text extraction, images need EXIF. fsPulse's `validate/` module had this pattern as per-format validators.

## What Happened

The salvage initially recommended "extend tree-sitter beyond Rust." The user corrected: tree-sitter is one extractor. LSP gives cross-file semantics. Arbitrary metadata extraction (YAML keys, PDF text, EXIF) is also needed. The architecture should be pluggable, not tree-sitter-centric.

## Override Protocol

For v0, a fixed set of extractors is fine. The interface between scanner and extractor should be generic enough that adding new extractors doesn't require scanner changes.
