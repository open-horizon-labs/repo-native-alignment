---
title: LSP diagnostic names must truncate by character
artifact_type: metis
date: 2026-07-16
outcomes:
  - context-assembly
tags:
  - lsp
  - diagnostics
  - utf-8
---

# LSP diagnostic names must truncate by character

Repository diagnostics are untrusted UTF-8 text. Even human-readable names and
log snippets must never shorten them with a byte range such as `message[..77]`.
Django's Pyright diagnostics contain a non-breaking space whose two-byte
encoding crossed that exact boundary, panicking a full enrichment after more
than six minutes of useful work and preventing the extracted graph from being
persisted.

Use character iteration (or an established UTF-8-safe truncation helper) for
display limits, preserve the full message in metadata, and seed regressions
with a multibyte character whose encoding straddles the old byte boundary.
