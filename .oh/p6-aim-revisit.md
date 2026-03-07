# P6: Aim Revisit — From Scaffolding to Semantic Context Assembly

**Date:** 2026-03-07
**Supersedes:** P5 implementation plan (oh_init + resolve_references framing)
**Outcome:** agent-alignment

## Why the P5 Framing Was Wrong

P5 proposed `oh_init` scaffolding and `resolve_references`. This was solving the wrong problem:

- **oh_init** assumes the bottleneck is bootstrapping `.oh/` structure. It's not — *finding relevant context is*.
- **resolve_references** assumes you have a reference to resolve. Agents don't start with references. They start with a *task* and need to find what's relevant.

Both are lookup tools. What's needed is *discovery*.

## Aim Statement

**Aim:** Agents automatically surface the right business context at the right phase of work, without requiring users to specify outcome IDs, file paths, or artifact names.

**Current State:** Skills preambles say "call `oh_get_outcomes` first." Agents must know *which* outcome to ask for. In practice they either list everything (floods context), require the user to name it (defeats automation), or skip it entirely (defeats alignment).

**Desired State:** When `/aim` runs, it describes the task and RNA MCP returns the 3-5 most relevant outcomes. When `/review` runs, it gets the guardrails that actually apply to this change. When `/salvage` runs, it writes metis that future semantic searches will find.

## Mechanism

**Change:** Add semantic search to RNA MCP tools, so each skill phase can *discover* relevant context by describing what it needs.

**Hypothesis:** The bottleneck isn't that agents lack instructions to pull context (preambles already tell them when). The bottleneck is that the only retrieval is exact-match (list all, get by ID, grep). Semantic search bridges the gap between "I'm working on auth refactoring" and "here are the outcomes, guardrails, and metis related to authentication."

## The Phase-Context Matrix

| Skill Phase | Needs | Search Query Pattern | Writes Back |
|-------------|-------|---------------------|-------------|
| `/aim` | Existing outcomes, related endeavors | `oh_search_context("outcomes related to [task]")` | New/updated outcome |
| `/problem-space` | Metis (what we've tried), constraints | `oh_search_context("learnings about [domain]")` | — |
| `/solution-space` | Metis, prior solution attempts | `oh_search_context("solutions tried for [problem]")` | Decision metis |
| `/execute` | Outcome progress, what's been done | `oh_search_context("progress on [outcome]")` | Signal records |
| `/review` | Guardrails, acceptance criteria | `oh_search_context("guardrails for [area]")` | — |
| `/dissent` | Guardrails + metis (constraints + failures) | `oh_search_context("risks and failures in [area]")` | — |
| `/salvage` | — (produces, doesn't consume much) | — | Metis + guardrail candidates |

**Key pattern:** every read is a semantic search, not an ID lookup. The agent describes what it's doing; RNA MCP finds what's relevant.

## What This Requires

### 1. Embedding Index Over `.oh/` Artifacts

LanceDB is already a Cargo dependency (commented out). We need:
- Index `.oh/` artifacts on write
- Embed on content + frontmatter (title, description, body all contribute)
- Incremental updates — re-embed when artifacts change

Embedding model: `fastembed-rs` (Rust-native, ONNX-based, local, no API key).

### 2. `oh_search_context` Tool

```
oh_search_context(
  query: string,             // natural language
  artifact_types: [string],  // filter: ["outcomes", "guardrails", "metis", "signals"]
  limit: int                 // default 5
) -> [{ artifact, relevance_score, snippet }]
```

Replaces "list all outcomes, then pick." Agent says "I'm working on auth" and gets back the 3 outcomes, 2 guardrails, and 1 metis most related to auth.

### 3. Auto-Embed on Write

Existing write tools (`oh_record_metis`, `oh_update_outcome`, etc.) compute and store embeddings as a side effect.

### 4. What We Do NOT Need

- **oh_init** — if `.oh/` is empty, `oh_search_context` returns empty. That's fine.
- **resolve_references** — subsumed. Search for what you mean, not what you name.

## Guardrails

- **Embedding model must be local.** No API keys, no network calls. Repo-native.
- **Latency <2s for search.** If slow, agents skip it.
- **Don't over-index.** `.oh/` is dozens of files, not millions. Quality > infrastructure.
- **Existing grep-based tools still work.** Semantic search is additive.
- **If embedding quality is poor,** fall back to LanceDB full-text search (TF-IDF, also supported).

## Implementation Sequence

1. Add `fastembed-rs` — local embedding computation
2. Index `.oh/` artifacts into LanceDB on server startup
3. `oh_search_context` tool — semantic search, filtered by artifact type
4. Auto-embed on write — write tools update the index
5. Update skill preambles — "call `oh_search_context` with your task description"
6. Test on this repo — run skills, verify context quality

Steps 1-3 are the minimum viable slice. Steps 4-5 close the loop.

## The Deeper Insight

P4/P5 treated alignment as a *data problem* (get the right files in place). The real framing is an *information retrieval problem* (find relevant context given a task description). Skills encode *when* to look. RNA stores *what* to find. The missing piece is *how* to match intent to artifacts. That's semantic search.

---

## Problem Space
**Updated:** 2026-03-07

### Objective
Each skill phase automatically discovers the right `.oh/` artifacts for its task without the user naming them.

### The validate-before-building Tension
The guardrail says "don't add infra before validating." P6 says "add embeddings." Resolution: the guardrail's override says "override when infra is the direct blocker." Semantic search IS the blocker — exact-match tools can't do discovery. The guardrail was right about LSP/multi-language. Wrong about retrieval.

### The Simpler Path Question
Do we need neural embeddings for 15 files? LanceDB supports full-text search (BM25) natively — no embedding model, still better than grep. For dozens of artifacts, BM25 may produce relevance as good as neural embeddings.

**Proposed escalation:** Start with LanceDB full-text search (BM25). Add `fastembed-rs` vector embeddings only if BM25 relevance is insufficient.

### Constraints
| Constraint | Type | Reason |
|------------|------|--------|
| Repo-native: no cloud API for search | hard | Core guardrail |
| Latency <2s | hard | Agent skips slow tools |
| `.oh/` corpus is small (dozens) | hard (current) | BM25 may suffice |
| LanceDB already a Cargo dep | soft | Just uncomment |

### Assumptions
1. BM25 full-text is sufficient for small `.oh/` corpus — if false: add embeddings
2. Agents will use `oh_search_context` — strong belief: preambles already say to
3. One search tool replaces "list all then filter" — if false: keep existing list tools

### Ready for /solution-space?
Yes. Two candidates: LanceDB BM25 (simpler) vs LanceDB + fastembed-rs (richer). Same tool surface either way.
