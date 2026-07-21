# Search projections and task context

RNA's unified search has two output projections:

- `agent` (the default) returns source-grounded context needed to act: stable
  identity and hydration handle, repository-relative location, symbol kind and
  signature, selection channel/reason, task role when known, body state, and a
  deterministic rendered-cost summary.
- `evidence` adds the audit trail: every contributing channel and rank, typed
  raw retrieval values, normalized contributions, deterministic tie-breaks,
  omission decisions, hashes, and capability diagnostics. It also lists every
  member of the bounded candidate set, including candidates omitted from the
  agent projection and the deterministic reason for each disposition.

The evidence projection changes presentation, not qualification. A caller can
therefore inspect why a result was selected without paying for that audit data
in every agent prompt.

## Ranking evidence

Product search combines channels only after deterministic within-channel
ranking. Every embedding result keeps two distinct values: the backend-native
value used for audit, and the product ranking score produced after the declared
normalization and optional test-path adjustment. Values with different meanings
are never added directly:

| Channel | Native value | Better direction |
| --- | --- | --- |
| exact/lexical | exact-match tier or lexical heuristic | higher |
| full text | BM25 score | higher |
| vector | cosine distance | lower |
| hybrid | reciprocal-rank-fusion relevance | higher |
| rerank | cross-encoder score | higher |
| structural | PageRank, edge degree, or structural heuristic | higher |
| graph | hop count or bounded graph heuristic | lower for hops, higher for heuristic |

RNA converts each available channel to a normalized rank contribution under a
named, versioned policy, then records the contributing channel ranks, final
ordering reason, and deterministic tie-break in the evidence projection. This
makes ranking invariant to positive rescaling of a scorer while allowing strong
exact or graph evidence to promote a candidate without letting weak graph
evidence dominate a substantially better semantic match. Separate FTS and
vector lanes are not claimed when the backend supplies only opaque hybrid RRF.
The sealed strict-semantic qualification path remains isolated: it accepts only
its frozen hybrid candidate set and reranker permutation and never gains a
product-search fallback.

### Fusion seam inventory

RNA normalizes evidence at the first shared-service seam where the channels
meet. A candidate may occur in several lanes; those observations are retained
independently, unioned before truncation, and fused exactly once.

| Producer/seam | Native meaning | Product treatment |
| --- | --- | --- |
| extracted exact/name lookup | match tier and deterministic lexical order | exact/lexical lane; stable identity closes ties |
| LanceDB keyword search | BM25 `_score` | native value is retained as `bm25`; product score uses non-negative saturation before optional test-path adjustment |
| LanceDB semantic search | cosine `_distance` | native distance is retained as `cosine_distance`; product score uses `max(1 - distance, 0)` before optional test-path adjustment |
| LanceDB hybrid search | backend `_relevance_score` | native value is retained as `hybrid_rrf_relevance`; product score uses non-negative saturation in one hybrid-RRF lane |
| lexical supplementation | deterministic text-match order | independent lexical lane; it cannot evict another lane before fusion |
| cross-encoder reranker | model relevance order | independent rerank lane in addition to its acquisition lane |
| structural ranking | PageRank/complexity/edge-degree heuristic | typed structural lane, never added as an unscaled raw bonus |
| graph traversal | typed relation, directness, and bounded hop count | graph lane ordered by directness/hops, then stable identity |
| task selector | role/facet coverage and final rendered cost | post-fusion maximum-coverage admission; not a relevance score |
| artifacts, commits, Markdown | exact/lexical or semantic document order | delivered through the same projection, or explicitly reported unavailable |
| strict semantic qualification | frozen hybrid candidate order plus reranker permutation | isolated legacy contract; no product lane, supplement, or fallback is introduced |

Evidence output records the actual producer used. For example, a hybrid request
that executes a vector fallback retains a native cosine distance and is labeled
vector, not hybrid; reranking adds a second contribution without erasing
acquisition provenance. If a non-strict backend omits its score column, the
deterministic substitute is labeled `deterministic_fallback`, never presented as
a backend-native observation. Test-path demotion is likewise recorded as an
explicit post-normalization adjustment rather than being mislabeled as a native
similarity or relevance score.

## Rendered cost

Cost is computed after canonical rendering. RNA reports UTF-8 bytes, Unicode
scalar values, and an explicitly named deterministic token estimate for the
complete response and for its headers, bodies, relationships, metadata, and
accounting footer. The estimate is a budgeting aid; it is never described as
provider usage or billed tokens.

Use `max_output_bytes` or `max_output_tokens` to set a final-output budget,
`max_body_bytes` to cap one record, and `max_total_body_bytes` to cap the
coalesced body section independently. The source planner degrades bodies only
through explicit states:

- `complete`
- `focused_span`
- `signature_only`
- `minified`
- `truncated`

`none` is a request policy: the rendered record is labeled `signature_only`
and carries an explicit `no_body_policy` omission rather than inventing a body
representation for source that was not emitted.

Every degradation has a stable omission reason and hydration handle. Partial
source is never labeled complete. Overlapping selected records are projected as
one source span with all satisfied identities, roles, and reasons attached, so
the same source byte is not emitted twice.

Source handles bind the exact root/path/line span and are revalidated against
the current filesystem. Evidence handles are short content-addressed references
to canonical capsules under `.oh/.cache/search_evidence/v1/`; the capsule binds
the original selection, query digest, channel evidence, and current-node
content digest. Missing, tampered, stale, oversized, or non-regular capsules
fail closed, and hydration performs no retrieval. The cache is derived local
state and is never part of frozen strict-semantic packets.

## Task context (opt in)

Set `context_mode=task` to compile a coding task into a bounded evidence bundle.
RNA resolves exact paths, locations, qualified names, backticked identifiers,
and named tests before running separate behavior, API, test, analogue, helper,
and graph-impact lanes. Exact hits, ambiguities, and misses remain distinct.

Callers may request bounded `context_roles`, `context_facets`, graph edge types
and hops, a body policy, and a render budget. Unknown roles/facets and values
above the service's hard limits fail closed. Selection maximizes newly covered
roles per marginal rendered cost; one span may satisfy several roles without
duplicating its source.

Example CLI request:

```bash
repo-native-alignment search \
  --repo . \
  --context-mode task \
  --context-roles editable_source,definition_or_api_state,test,direct_dependency,caller_or_impact \
  --body-policy focused_span \
  --max-output-tokens 4000 \
  "Fix source_span so an out-of-range end line reports the valid bound"
```

The MCP `search` tool accepts the same service fields and returns the same
canonical rendering.

## Graph-delta beta (opt in)

`context_mode=graph-delta-beta` accepts a bounded unified diff or structured
edit sketch in `proposal`. RNA parses it into an ephemeral overlay, compares
deterministic before/after routes, and reports changed edges, lost reachability,
equal-cost alternatives, bypassed behavior, affected contracts/tests, and an
affected-locus checklist.

The beta analyzer never applies the proposal, changes the worktree, mutates the
published graph, or writes overlay evidence to LanceDB. Every claim links to an
existing source span or a proposal line. Unsupported, ambiguous, binary,
traversal-containing, oversized, or incomplete inputs fail closed. Missing
optional graph/LSP/semantic evidence is instead reported as a degraded
capability; it is never presented as proof that no impact exists.

## Strict semantic qualification

The sealed SWE-bench strict path keeps its frozen qualification, ordering,
rendering, protocol, and packet bytes. Product projections, calibrated fusion,
task context, and graph-delta are separate opt-in/non-strict paths and do not
retroactively change frozen benchmark evidence.
