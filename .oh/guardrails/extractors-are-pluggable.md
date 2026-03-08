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

## Validated In Practice

This batch (PRs #35, #43, #44) shipped 7 new tree-sitter language extractors (Ruby, C++, C#, Kotlin, Zig, Lua, Swift) and LSP-as-extractor without touching the scanner. The pluggable interface held. Adding a new language extractor is now a self-contained PR.

## Override Protocol

The interface between scanner and extractor must remain generic. A new extractor must not require scanner changes. If a proposed extractor requires scanner changes, refactor the interface first.
