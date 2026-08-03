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

## Rendered delivery budget

Byte and token limits apply to the complete self-accounted response, including
headers, metadata, bodies, relationships, omissions, and the accounting footer.
Selection and rendering use the same bounded fitter. When optional
server-owned detail does not fit, RNA degrades it deterministically: capability
diagnostics, candidate audit, capability-list detail, and then the omission
list, which collapses into one entry reporting `omitted_detail_count` while
retaining one compact hydration handle. It next degrades per-record evidence
and metadata, relationship detail/rows, task bodies to obligation excerpts,
remaining source bodies, and finally the lowest-ranked flat-search tail.
New degradation omissions re-enter the same ladder and are compacted before
later evidence-bearing stages. Task-selected records are
protected because a role or task obligation is covered only when its actionable
carrier remains in the final fitted packet.

Every retained record whose detail can be omitted keeps a stable
`rna-h2` version-2 source or evidence handle. V2 is a compact, checksummed,
self-describing encoding; the hydration endpoint continues to accept existing
V1 handles. Budgets below the compact header/result/handle/accounting envelope
still fail with a typed `BudgetTooSmall` error rather than returning an empty or
misleading success.

Convergence diagnostics obey the same complete byte/token caps. When even the
smallest self-accounted noninjectable diagnostic cannot fit, the service
returns a typed `BudgetTooSmall` delivery error and the MCP adapter exposes it
as a tool error; it never emits an over-budget diagnostic or consumes the
one-time business-context preamble.

The minimum envelope is measured, not a repository-wide magic number because
stable identity and path lengths vary. For a request with candidates it is the
exact final `RenderCost` of the fully degraded fixed scaffold plus the
lowest-cost actionable record identity, compact obligation certificate,
source handle, and self-accounting footer. The bounded fitter computes that
same value during admission; if neither caller limit can hold it,
`BudgetTooSmall.minimum` reports the final byte/token cost of that irreducible
plan. Tests freeze this definition and the degradation order rather than one
incidental global byte count.

Generic query-concept and query-affine structural-profile obligations apply to
every language. Structural profiles distinguish independently useful evidence
branches by role and matched query concepts, without embedding repository- or
language-specific vocabulary in the selector.

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

### Representative four-query channel diagnostic (#812)

This is a deterministic contract diagnostic, not a latency, quality, or model
benchmark. The “before” column records the scale/pathology found by the packet
and seam audit; the “after” column is asserted by the named pure/service
fixtures. No unexecuted query is presented as measured performance.

| Representative query shape | Before the calibrated contract | After, as asserted by fixtures |
| --- | --- | --- |
| Exact name: `projected_search` | Exact and later semantic/graph observations could be assembled at different seams without one comparable contribution record. | Exact/lexical is an explicit ranked lane, additional lanes remain visible, and stable identity closes ties. `giant_semantic_decoy_is_bounded_and_cannot_erase_exact_graph_evidence` proves a large semantic decoy cannot erase the exact/graph candidate solely through score scale. |
| Natural language: “paginate source hydration without losing bytes” | Native semantic or reranker magnitudes could dominate merely because their numeric scale was larger. | Semantic acquisition and rerank are independent within-channel ranks. `multiplying_each_channel_alone_cannot_change_fusion` proves positive raw-score rescaling cannot change identities or order when channel order is unchanged. |
| Graph entry: impact from `projected_graph_delta` | The audited formula admitted million-point adjacent semantic gaps while graph bonuses remained below one hundred thousand, so direct graph evidence could not promote by one semantic position. | Graph directness/hops contributes through the same rank fusion. `graph_only_evidence_promotes_while_weak_graph_does_not_overwhelm_semantic` proves both legitimate promotion and weak-graph restraint. |
| Mixed task: change `projected_graph_delta` and verify its test | A flat top-k could spend the body budget on a giant generic semantic record and omit a named test or state/API obligation. | Fusion establishes comparable candidates first; task selection then admits distinct exact, editable, test, state/API, analogue, dependency, and impact roles by coverage per rendered cost. Exact misses, ambiguities, and role omissions remain explicit. |

The table describes non-strict product search only. The sealed #779 strict
semantic path still consumes its frozen hybrid candidate order and reranker
permutation without lexical, graph, vector-only, CPU, or original-order
fallback.

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
roles and generic task obligations per marginal final-packet cost; one
actionable record or span may satisfy several obligations without duplicating
its source. Punctuation-only constants are rejected, while incomplete spans,
unrelated graph neighbors, and generic tests cannot satisfy coverage merely by
carrying a nominal role.

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

The pure parser emits proposal-grounded relationship facts for calls, imports,
registrations, and qualified attribute/state references. These are candidates,
not graph claims: the live adapter must resolve each target to exactly one
current source-grounded node before adding or removing an overlay edge.
Ambiguous or absent endpoints degrade capability evidence rather than being
guessed. Changed lines also surface grounded behavioral classes for branches,
reconciliation, representation handling, error paths, and state propagation.
For route changes, bounded traversal returns only the nearest source-grounded
test layer; farther tests are not mislabeled as equally direct.

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
