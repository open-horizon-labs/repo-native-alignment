# RNA as a Future Context-Assembler Source

**Status:** Draft
**Updated:** 2026-03-07

## Why this exists

RNA is being expanded from a repo-local `.oh/` intelligence layer into a broader workspace code/context engine. The current roadmap in issues #5 and #7-#12 is directionally right for local agent use, but it risks coupling extraction, local storage, and future integration too tightly.

This document steers RNA so it can do two jobs well:

1. **Now:** serve local MCP clients with fast repo/workspace intelligence
2. **Later:** register cleanly as a first-class source for Context Assembler without rewriting the scanner/extractor stack

The constraint is simple:

- RNA must remain useful as a standalone local runtime
- RNA must not hard-code assumptions that belong to Context Assembler's centralized read model

## Architectural stance

Treat RNA as a **source-capable local intelligence engine**.

That means separating three concerns now:

1. **Local extraction and query runtime**
   - scanner
   - tree-sitter and schema extractors
   - LSP enrichment
   - local LanceDB graph and search
2. **Source-owned canonical output model**
   - versioned source declaration
   - canonical event envelope
   - deterministic IDs and idempotency
3. **Optional projection transport**
   - local outbox today
   - FEED/Event Hub publisher later

This follows the Context Assembler boundary:

- sources own `src_*` write models
- assembler owns `assembly_*` read models
- assembler hot path reads only `assembly_*`

## What this implies for RNA

### 1. LanceDB is RNA's local read model, not its public contract

Issue #11 is correct to use LanceDB as the local graph/query store. It should **not** become the only durable integration contract.

If future integration depends on RNA's internal LanceDB schema, we will couple:

- extractor evolution
- local query optimizations
- centralized projection logic

Instead:

- RNA owns its local storage layout
- RNA exposes a stable source declaration and canonical emitted records
- Context Assembler consumes the emitted records, not RNA's internal tables

### 2. Extraction and projection must be different layers

Issues #8-#10 describe extractors that produce nodes, edges, chunks, schemas, and enrichments. Keep that.

But do not let extractor output be equivalent to projection output.

Recommended layering:

1. scanner detects changed files/roots
2. extractor builds semantic model
3. mapper converts semantic model into canonical source records
4. local projector writes RNA's LanceDB graph/search indexes
5. optional publisher appends same canonical records to outbox

This lets RNA stay fast locally while making future source registration mostly a transport and projection concern.

### 3. RNA should own source payloads, not assembler read semantics

Do **not** force `assembly_facts` or `assembly_vectors` shapes into RNA internals.

RNA should own:

- scan state
- extractor payloads
- local graph schema
- local embeddings if helpful
- per-root configuration

Context Assembler should own later:

- projection into `assembly_*`
- retrieval policy
- cross-source ranking
- centralized prompt assembly

## Design principles

### Source-first, not assembler-coupled

RNA should be buildable, testable, and valuable even if Context Assembler never ships.

### Deterministic replay over incidental cache state

If RNA cannot replay its own extracted facts deterministically, it will be a brittle source later.

### Local-first latency

The local MCP path must not regress because of future-source concerns. Source compatibility must be additive.

### Evolve payloads behind a stable envelope

Lock the canonical envelope early. Keep source payloads evolvable.

### Backfill and rollback are product requirements

A source without replay, idempotency, and kill switches is not production-safe for assembler integration.

## Proposed source shape

Start with one source:

- `source = code.workspace`
- `version = v1`

This is intentionally broad. It keeps the first integration surface small.

Possible later split if useful:

- `code.symbols`
- `code.schemas`
- `code.topology`
- `code.artifacts`

Do not split early unless it materially simplifies ownership or retrieval policy.

## Canonical envelope

RNA should emit canonical records/events with a stable cross-system envelope, regardless of how its internal LanceDB schema evolves.

```json
{
  "envelope_version": "1",
  "event_id": "evt_01...",
  "source_event_id": "code.workspace:symbol:sha256:...",
  "event_type": "fact.upsert",
  "user_id": "u_local",
  "source": "code.workspace",
  "source_schema_version": "v1",
  "fact_type": "code.symbol",
  "event_time": "2026-03-07T12:00:00Z",
  "ingested_at": "2026-03-07T12:00:01Z",
  "partition_bucket": "202603",
  "idempotency_key": "sha256(...)",
  "payload": {},
  "extensions": []
}
```

### Lock now

These fields should be mandatory and stable:

- `envelope_version`
- `event_id`
- `source_event_id`
- `event_type`
- `user_id`
- `source`
- `source_schema_version`
- `fact_type`
- `event_time`
- `ingested_at`
- `partition_bucket`
- `idempotency_key`
- `payload`
- optional `extensions[]`

### Keep source-owned and evolvable

These should remain RNA-owned and versioned by source declaration:

- payload shape
- local indexing strategy
- local storage schema
- optional vector content
- extension capsule details

## Fact families for `code.workspace:v1`

Recommended initial fact types:

- `code.symbol`
- `code.import-edge`
- `code.topology-edge`
- `code.schema`
- `code.schema-field`
- `code.reference-edge`
- `code.diagnostic`
- `code.chunk`
- `workspace.root`
- `workspace.scan`

These should be treated as source facts, not as direct `assembly_*` row shapes.

## Deterministic identity and idempotency

Every emitted record must have a deterministic identity.

Examples:

### Symbol fact

`sha256(root_id | file_path | symbol_kind | symbol_name | signature_hash | schema_version)`

### Edge fact

`sha256(root_id | source_fact_id | relationship | target_fact_id | schema_version)`

### Chunk fact

`sha256(root_id | file_path | chunk_kind | byte_range | content_hash | schema_version)`

### Scan fact

`sha256(root_id | scan_mode | snapshot_marker | schema_version)`

This enables:

- duplicate-safe backfill
- replay after projector bugs
- incremental rescans without logical duplication
- safe future FEED publishing

## Extension capsules

Use typed extensions for extractor-specific enrichments rather than mutating the core envelope.

Examples:

- `code.semantic@v1`
- `code.topology@v1`
- `code.schema.constraints@v1`
- `code.provenance@v1`

Rules:

- core fact remains valid without extensions
- unknown extensions are non-fatal
- extensions carry extractor-specific or derived detail
- future Context Assembler catalog registration can validate them independently

## Local outbox seam

Add a local outbox now, even before remote publishing exists.

Recommended abstraction:

```rust
trait RecordSink {
    fn project_local(&self, records: &[SourceRecord]) -> Result<()>;
    fn append_outbox(&self, records: &[SourceRecord]) -> Result<()>;
}
```

### Initial behavior

- `project_local`: update RNA LanceDB tables/indexes
- `append_outbox`: append canonical records to a local durable log/table

### Future behavior

- a publisher reads the outbox and emits to FEED/Event Hub
- no extractor rewrite required
- no scanner rewrite required

This is the key seam that preserves today's local architecture while making tomorrow's source transport cheap.

## Replay and backfill model

RNA should support these flows explicitly:

1. full workspace reindex
2. root-only reindex
3. changed-files reindex
4. rebuild local graph from canonical records
5. replay outbox from checkpoint
6. backfill historical state into canonical records

If replay is not first-class, source registration later will become a one-way door with poor recovery.

## Root and scope model

Issue #12 should define more than config. It should define source scope.

Each record should carry stable scope metadata such as:

- `workspace_id`
- `root_id`
- `root_type`
- `repo_id` when git-aware
- `branch` when relevant
- `commit_sha` when relevant
- `file_path`
- `language`

This makes RNA records useful for both:

- local MCP filtering and traversal
- future assembler retrieval policies and source scoping

## Provenance requirements

Every fact should preserve enough provenance for debugging, replay, and trust:

- extractor name
- extractor version
- file path
- byte or line range
- root and repo identity
- commit SHA or mtime snapshot
- scan timestamp
- confidence where inference is heuristic

Without strong provenance, graph quality issues will be hard to diagnose later.

## Relationship to the current roadmap

### Issue #5 — deterministic bootstrap

Keep the current goal, but extend acceptance so bootstrap also validates the source boundary:

- source declaration present and versioned
- local outbox initialized
- tiny replay smoke test passes
- source health surfaced in verify output

### Issue #7 — scanner

Interpret scanner output as **deterministic source deltas**, not just changed files.

Add expectations for:

- root-aware scope identifiers
- stable snapshot markers
- change events suitable for replay/backfill

### Issue #8 — tree-sitter extractor

Tree-sitter remains the always-available baseline.

Add expectations for:

- stable semantic unit IDs
- canonical record mapping layer
- topology signals emitted as facts/extensions, not only direct graph rows

### Issue #9 — LSP enrichment

LSP should enrich existing entity identities, not redefine them.

Add expectations for:

- merge semantics onto tree-sitter-established entities
- separate enrichment provenance
- non-blocking upgrades that preserve deterministic replay

### Issue #10 — schema extraction

Schemas are strong candidates for future cross-source retrieval value.

Add expectations for:

- explicit schema fact families
- stable contract IDs
- migration/evolution edges with replay-safe IDs

### Issue #11 — unified graph

Keep LanceDB as the local graph/query engine.

Refine the scope:

- local graph tables are RNA internals
- canonical emitted records are the future source contract
- graph rebuild from canonical records should be possible

### Issue #12 — multi-root

Treat this as the scope model for source registration later.

Add expectations for:

- stable root IDs
- root-type-aware defaults
- cross-root provenance and filtering
- per-root enable/disable for future retrieval policy

## Recommended issue strategy

Do **not** create a parallel second set of implementation issues for the same work. That will fragment ownership and confuse sequencing.

Instead:

1. **Add this design doc** as the architectural steering artifact
2. **Create one new cross-cutting umbrella issue** for source compatibility
3. **Update or comment on issues #5 and #7-#12** with issue-specific acceptance additions and a link back to this doc

Recommended new umbrella issue:

- `Architecture: make RNA a source-capable local engine for future Context Assembler integration`

That umbrella should track the cross-cutting concerns that do not belong to any single extractor issue:

- source declaration `code.workspace:v1`
- canonical envelope
- deterministic ID and idempotency rules
- local outbox seam
- replay/backfill requirements
- extension capsule policy

Then keep the existing issues as the implementation slices.

### Why not only comments on existing issues?

Because the source-compatibility work is not reducible to one issue and is easy to lose in scattered comments.

### Why not a whole new issue tree?

Because that creates duplicate project management for the same roadmap. The extractor/scanner/graph issues are still the right execution decomposition.

## Suggested implementation order

1. Lock `code.workspace:v1` source declaration
2. Lock canonical envelope fields
3. Implement deterministic IDs and idempotency helpers
4. Add local outbox abstraction
5. Make scanner and extractors emit canonical source records
6. Keep local LanceDB graph as a projection of those records
7. Add replay and rebuild commands/tests
8. Later, add FEED/Event Hub publisher

## Acceptance criteria for this design direction

RNA is on the right path when all of the following are true:

- scanner/extractors work without any dependency on Context Assembler runtime choices
- local MCP queries remain fast and standalone
- canonical source records exist independently of local LanceDB layout
- local graph can be rebuilt from canonical records
- deterministic replay produces no logical duplicates
- roots, facts, and edges carry stable scope and provenance
- future FEED publishing can be added without rewriting extractors

## Non-goals

This document does **not** recommend:

- introducing Cosmos DB into RNA
- forcing `assembly_*` schemas into RNA internals
- delaying local usefulness until centralized infrastructure exists
- replacing LanceDB as RNA's local graph/query engine
- coupling RNA to assembler retrieval logic or prompt assembly

## Bottom line

Build RNA as a clean producer with a strong contract and a useful standalone runtime.

That preserves what RNA wants now:

- fast local MCP behavior
- repo/workspace scanning
- graph-aware code understanding
- multi-root discovery

And it preserves what Context Assembler will need later:

- source-owned writes
- replay-safe projections
- idempotent backfill
- kill-switchable retrieval onboarding
- stable, versioned source contracts
