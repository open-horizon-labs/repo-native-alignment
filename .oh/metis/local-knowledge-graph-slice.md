---
title: Local knowledge graph slice
date: 2026-06-26
outcome: context-assembly
---

# Local Knowledge Graph Slice

The first experimental RNA local-knowledge graph slice should extend graph mechanics, not hardcode domain ontology.

Implementation learning:

- `NodeKind::Other(String)` was already enough for repo-local/domain-specific node kinds.
- `EdgeKind` needed the same escape hatch: repo-local relationships must survive as true edges, not metadata.
- Persist/load was the risk point because LanceDB stores `edge_type` as a string but unknown strings were previously dropped by `parse_edge_kind`.
- A minimal source-first convention works for the first slice: markdown YAML frontmatter can declare `rna.kind`, `rna.id`, optional metadata, and relationships to other local knowledge nodes.
- The fixture that matters is not just node creation. It must answer a review question: what supports this claim, and which manuscript node consumes it?

Guardrail reinforced:

Do not add book-specific concepts such as `Quote`, `Claim`, `Chapter`, or `Supports` as RNA core enum variants. Keep vocabulary local; keep graph persistence/traversal/search mechanics generic.

Review follow-up:

- Persisting `rna.metadata.*` is not sufficient. MCP formatting must render those fields in both compact and full search results so agents can discover them.
- A shared permissive edge-label parser must not weaken context-specific validation. Boundary configuration remains strict to `Produces` and `Consumes`; markdown frontmatter is the extension point for repo-local relationship labels.
- Regression coverage should assert delivered search text and reject misspelled boundary kinds, not only inspect extracted or persisted graph records.
