---
id: session-1-salvage
outcome: agent-alignment
title: 'Session 1 Salvage: Initial MCP Server Build'
---

## Key Learnings

1. **Protocol version mismatch silently hangs MCP clients** — rust-mcp-sdk rejects but doesn't respond. Must match client's version (2025-11-25).
2. **"Intersection query" was a lie until `outcome_progress`** — renaming `query` → `search_all` and building the structural join was the turning point.
3. **Structural joins > semantic search for the core use case** — `outcome_progress` follows links, doesn't need embeddings. Embeddings needed for discovery, not joins.
4. **Body matching in code search is noise** — restrict to name + signature.
5. **Adoption friction is in tooling setup, not file creation** — `.mcp.json`, binary builds, frontmatter schemas are the real barrier.
6. **The feedback loop is the product** — read tools alone don't prove the thesis. Write tools (`oh_record_signal`, `oh_update_outcome`) close the loop.
7. **Test with real MCP client, not curl** — protocol negotiation differs.

## Guardrails Discovered

- Test MCP changes with TypeScript SDK client, not just curl
- Name tools honestly (what it does, not what you wish it did)
- Don't add extractors before validating the hypothesis

## Next Session Should

1. Test on a second, non-trivial repo
2. Measure agent-scoping-accuracy SLO
3. Reduce cold-start friction (oh_init command)
