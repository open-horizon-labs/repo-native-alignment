# Release v0.1.15 Decision Package
**Started:** 2026-03-22
**Binary:** repo-native-alignment 0.1.15

---

## Commits since v0.1.14

67 commits. Major features:

| PR | Feature |
|----|---------|
| #500 | EventBus + ExtractionConsumer trait (decoupled pipeline) |
| #496 | Append-only LanceDB with scan_version |
| #495 | WAL sentinels (extract_completed.json, lsp_completed.json) |
| #494 | Generic .oh/extractors/ config-driven Produces/Consumes edges |
| #493 | PostExtractionRegistry plugin architecture |
| #491 | Aho-Corasick automaton for TestedBy pass (O(N) vs O(T×P)) |
| #490 | Incremental cross_weights maintenance in Louvain |
| #488 | Rayon parallelization for per-file extraction |
| #487 | Background scanner runs post-extraction passes |
| #484, #482 | Subsystem nodes in petgraph + incremental path |
| #481 | pub/sub + SSE/WebSocket extractors + extractor gating |
| #480 | Framework detection pass (NodeKind::Other("framework")) |
| #473 | Subsystem detection (NodeKind::Other("subsystem")) |
| #472 | Cross-file call detection via import resolution |
| #443 | directory-module pass for unconditional BelongsTo edges |

Also: unit tests PR #501 (CodeRabbit-generated), ADR docs for event bus architecture.

---

## Test Results

**51 passed, 0 failed, 4 skipped**

Run: `bash scripts/test-suite.sh` (no IC repo) → 41 passed, 0 failed, 1 skipped
Run: `bash scripts/test-suite.sh $RNA_REPO $IC_REPO` → 51 passed, 0 failed, 4 skipped

Skipped tests are pending features not yet merged (#465, #466, #492, #502).

### Test Groups
- Core Search: 8 tests (symbol, FTS, kind filters, subsystem filter)
- Edge Traversal: 3 tests
- list-roots: 2 tests
- Scan Performance: 1 test (incremental < 120s — actual: ~1.5s)
- WAL Sentinels: 4 tests (extract_completed.json fields)
- Append-only LanceDB: 1 test (schema_version >= 17)
- PostExtractionRegistry: 3 tests (structural)
- EventBus/ExtractionConsumer: 5 tests (structural + ADR constraint)
- Custom Extractor Config: 3 tests (structural)
- Cross-File Calls: 3 tests
- BelongsTo Edges: 2 tests
- Framework Detection: 3 tests
- Subsystem Nodes: 3 tests
- Innovation-Connector: 10 tests (conditional on IC cache)
- Pending (SKIP): 4 tests

---

## Smoke Regression Candidates

Top 5 tests to promote from full suite to smoke:

1. **WAL sentinel: extract_completed.json has schema_version** — catches any scan pipeline regression that breaks persistence. After any scan, this file MUST exist with valid fields. Zero false-positives; catches the most critical invariant.

2. **Subsystem nodes present on RNA repo** — `search '' --kind subsystem` verifies that post-extraction passes ran and emitted first-class subsystem nodes. Catches subsystem_pass regression and PostExtractionRegistry wiring.

3. **Framework detection: lancedb detected** — `search '' --kind framework` on the RNA repo. Verifies framework_detection_pass is wired and the RNA repo's own LanceDB dependency is detected.

4. **cross-file calls symbol present** — `search 'import_calls_pass'` verifies that the import_calls pass ran and its own function is indexed. Self-referential: the pass must run to index itself.

5. **BelongsTo: embed.rs function belongs to module** — `graph --node ... --edge-types belongs_to` verifies both directory_module_pass ran AND graph query CLI reads correctly from cache. Exercises the full read path.

Rationale: These 5 cover the four major v0.1.14→v0.1.15 architectural additions (EventBus/PostExtractionRegistry wiring, WAL sentinels, subsystem/framework nodes, cross-file edges) and the core read path. All are fast (~1s each), no IC repo needed.

---

## Release Notes Draft

### v0.1.15 — What agents can do now that they couldn't before

#### #context-assembly

**Before:** Agents querying for architectural relationships (which modules own which functions, which frameworks power which service) had to reason from file paths and naming conventions. RNA returned functions and classes but not their structural homes.

**After:** Every function and class has explicit `BelongsTo` edges to its module hierarchy and subsystem. Framework nodes (`lancedb`, `tokio`, `fastapi`, `react`) are first-class queryable entities with `UsesFramework` edges. An agent asking "what does the embedding subsystem depend on?" gets a real answer from the graph, not from reading 50 files.

#### #subsystem-detection

**Before:** Subsystem detection produced only metadata annotations; subsystems were not searchable as first-class nodes. `search --kind subsystem` returned nothing.

**After:** `search --kind subsystem` returns first-class `NodeKind::Other("subsystem")` nodes. Agents can enumerate all detected subsystems, find which symbols belong to each, and navigate subsystem boundaries explicitly. Framework detection adds `NodeKind::Other("framework")` nodes with the same semantics.

#### #domain-context-compiler

**Before:** Adding custom extraction logic (e.g., message broker topics, infrastructure edges) required writing Rust code and rebuilding RNA.

**After:** Drop a `.oh/extractors/*.toml` file in any repo. RNA reads it at scan time and emits `Produces`/`Consumes` edges for any message broker or custom dependency pattern. No Rust, no build, no release required. The agent authors the domain model; RNA runs it.

#### Pipeline reliability improvements

- WAL sentinels (`extract_completed.json`) mark when each scan phase finishes. Agents and the background scanner can distinguish "scan in progress" from "scan never ran."
- Append-only LanceDB with `scan_version` column means partial writes no longer corrupt the index. Each scan version is self-consistent and stale versions are compacted automatically.
- PostExtractionRegistry unifies all post-extraction passes behind a single call site. Previously, passes ran from scattered call sites and could be missed in the incremental path.
- EventBus and ExtractionConsumer trait decouple pipeline stages. Foundation for the Phase 3 event-driven architecture described in `docs/ADRs/001`.

#### Performance

- Rayon parallelization of per-file extraction (all cores used during tree-sitter pass)
- Aho-Corasick automaton for TestedBy pass: O(N) vs O(T×P) substring matching
- Incremental Louvain: cross_weights maintained incrementally, no full recompute on each scan

---

## Breaking Changes

- **SCHEMA_VERSION bumped to 17** (adds `scan_version` column). First scan after upgrade triggers full rebuild from LanceDB (schema mismatch auto-recovery). No manual action needed.
- **EXTRACTION_VERSION bumped to 10**. Cached extraction results are invalidated; full re-extraction runs automatically on next scan.

---

## Open Issues

### Block release
None identified.

### Safe to ship with
- #465 (OpenAPI bidirectional) — not yet merged, skip list in test suite
- #466 (gRPC service edges) — not yet merged, skip list in test suite
- #492 (module split) — not yet merged, skip list in test suite
- #502 (pipeline wired to EventBus) — Phase 3 work, not blocking Phase 2 release
- Lance panic during rapid successive incremental scans — transient, no data loss, self-resolving. Manifests as "JoinError::Cancelled" in stderr. All scan data is persisted before the panic.

---

## Recommended Version Bump

**MINOR** (0.1.14 → 0.1.15): new queryable node kinds (subsystem, framework), new CLI behavior (graph reads from LanceDB), schema bump. No API removals. Aligns with current version already in Cargo.toml.

---

## Decision

RELEASE / TWEAK / NOT — awaiting human decision.
