# Content-source contract

This document is the normative source-of-truth contract for content-native
facts. It governs the Markdown AST, evidence provenance, and domain-pack work
tracked by issues #713–#715. The adversarial corpus in
`tests/fixtures/content_source_contract/` is part of this contract.

## Invariant

A relationship is `confirmed` only when a current repository body span supports
it. Frontmatter, sidecars, registries, filenames, headings, asset counts, and
broad topical similarity may nominate a candidate; none is relationship truth.

Every confirmed fact MUST be reproducible from the checked-in source bytes and
MUST become non-confirmed when those bytes no longer validate. A consumer that
cannot prove this invariant MUST emit a diagnostic instead of silently omitting
or confirming the fact.

## Evidence selector

Every evidence-bearing fact carries one or more selectors with these fields:

| Field | Requirement | Meaning |
|---|---|---|
| `file_path` | required | Normalized, repository-relative path; no absolute paths or `..`. |
| `line_start`, `line_end` | required | One-indexed, inclusive range in the current file. |
| `byte_start`, `byte_end` | required for content-native Markdown | Zero-indexed, half-open UTF-8 byte range. A Markdown fact without both cannot be `valid` or `confirmed`. |
| `body_node_id` | required | Stable AST body-node identity, never a frontmatter/sidecar node ID. |
| `snippet_hash` | required | Lowercase BLAKE3 digest of the exact bytes in the selected half-open range. |
| `extractor_id` | required | Versioned generic extractor identity. |
| `pack_id` | required for domain interpretation | Versioned repo-local pack identity; absent only for generic AST facts. |
| `rule_id` | required for derived facts | Stable, pack-scoped rule identity. |
| `confidence` | required | `detected` or `confirmed`; only validated body evidence can be `confirmed`. |
| `validation_status` | required | One of the states below. |

Line and byte ranges identify the same content. Content-native Markdown is read
from repository bytes, so its extractor MUST preserve byte offsets even if its
parser API reports only lines. An implementation MUST verify line/byte agreement.
`snippet_hash` always hashes the unmodified bytes in `[byte_start, byte_end)`;
there is no line-normalized or platform-normalized fallback.

`body_node_id` provides identity;
location may change after an edit, but identity alone never validates evidence.
Its canonical UTF-8 encoding is:

```text
<file_path>::body::explicit:<percent-encoded-inline-id>
<file_path>::body::ast:<kind>[<zero-based-sibling-ordinal>]/...
```

An explicit inline ID takes precedence and may preserve identity across a move;
duplicate explicit IDs are invalid. Otherwise, each AST segment uses the generic
Markdown node kind plus its ordinal among same-kind siblings under its parent.
RFC 3986 percent encoding is uppercase and applies to bytes outside the unreserved
set. The root document is omitted from the AST path. Moving a node without an
explicit ID changes its identity; changing only text or source location does not.

For multi-span evidence, store an ordered, non-empty selector list. Edge identity
MUST distinguish equal source/target/kind triples supported by materially
different evidence. A display snippet is derived from the selected bytes and is
not itself authoritative.

## Validation states and transitions

| State | Meaning | May back a graph edge? |
|---|---|---|
| `valid` | Selector resolves; ranges agree; hash matches; rule validates the body node. | Yes, at declared confidence. |
| `stale` | Identity resolves, but location/hash/rule verification no longer matches the current body. | Candidate only; never confirmed. |
| `unresolved` | File, body node, anchor, or referenced body evidence cannot be resolved. | No. |
| `invalid` | Selector or rule is malformed, forbidden, or points outside body content. | No. |

On every scan of a changed source file, dependent evidence MUST be revalidated
before its relationship is exposed. `valid -> stale|unresolved|invalid` removes
confirmed status in the same committed graph version. Cached or independently
persisted graph records MUST NOT survive that transition as confirmed.

## Diagnostic contract

Diagnostics have a stable `code`, `severity`, source `file_path`, optional
selector, and human-readable message. The corpus locks these codes:

| Code | Severity | Condition |
|---|---|---|
| `content.metadata_without_body_evidence` | error | Frontmatter nominates a fact with no supporting body selector. |
| `content.sidecar_without_body_evidence` | error | A sidecar/registry nominates a fact with no supporting body selector. |
| `content.missing_body_evidence` | error | A rule requires evidence but selects no body node. |
| `content.unresolved_anchor` | error | A Markdown/body anchor does not resolve exactly. |
| `content.orphan_quote` | error | A quote is registered or attributed without an exact body quote span and resolvable source. |
| `content.visual_not_evidence` | error | A screenshot/image is accepted by presence, count, filename, or broad topical fit rather than an anchored body claim/caption. |
| `content.stale_verification` | error | Stored verification/hash no longer matches current selected bytes. |
| `content.public_vocabulary_leak` | error | Public chapter prose exposes pack-internal worksheet/taxonomy headings as authored content. |
| `content.duplicate_body_id` | error | More than one body node declares the same explicit inline ID in a file. |

Pack-specific diagnostics MAY add namespaced codes. The generic runtime owns the
diagnostic shape and lifecycle; a pack supplies domain-specific detection (for
example, which headings are private worksheet vocabulary) without hardcoding
that vocabulary in RNA core. Packs MUST NOT weaken or replace these boundary
diagnostics, and invalid pack configuration MUST be diagnosed rather than
silently skipped.

## Frontmatter and sidecars

Frontmatter is a document identity and metadata seed. It may provide stable
document IDs, titles, authorship, dates, pack selection, or candidate references.
It MUST NOT, by itself, confirm a relationship or satisfy evidence validation.

Sidecars and registries may configure packs, declare vocabulary, or nominate
candidates. They MUST NOT introduce an independent graph source of truth. Every
content-derived node or edge they nominate must resolve to current body-node
selectors and pass the same validation transitions as facts discovered directly
from the AST.

## Custom-edge compatibility

This contract supersedes frontmatter-only local-knowledge declarations; it does
not discard the custom-edge infrastructure shipped by #707/#710.
`EdgeKind::Other(label)`, traversal, filtering, persistence, weights, and render
labels remain the generic transport. The change is at admission and provenance:

1. metadata or a pack nominates a custom-edge candidate;
2. a pack rule binds it to one or more body selectors;
3. validation determines status and confidence;
4. only valid evidence may produce a confirmed custom edge;
5. persistence and MCP rendering retain selectors, status, and diagnostics;
6. source edits synchronously invalidate or downgrade dependent edges.

Domain vocabulary belongs to repo-local packs. RNA core owns generic AST node
kinds, selector validation, lifecycle states, and diagnostics—not book-specific
strategy, tactic, support, quote, chart, or worksheet enums.

## Conformance

An implementation conforms only if it passes every case in the adversarial
corpus, including the positive body-backed control. Counts, registry
completeness, a plausible chapter match, a heading-only match, bibliography-only
presence, or a decorative asset MUST NOT substitute for selected body evidence.
