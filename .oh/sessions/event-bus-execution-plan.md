# Event Bus Execution Plan
**Goal:** RNA becomes a thin bootstrap + event bus + consumer registry. All extraction, enrichment, embedding, and persistence are independent consumers. See `docs/ADRs/001-event-bus-extraction-pipeline.md`.

**DO NOT STOP until all phases are complete.**

---

## Currently Running

| Agent | Issue | Status |
|-------|-------|--------|
| aa1aba7... | #478 PostExtractionRegistry | running |
| ac76b7d... | #468 custom extractor config loader | running |
| aced42b... | #475 WAL sentinels | running |
| a9ff8fb... | #477 append-only LanceDB | running |

---

## Phase 1: Foundation (current)

### On #478 merge → immediately queue:
1. **ADR audit agent** — run all 5 constraint checks from ADR
2. **#479 agent** — full event bus + migrate ALL passes to consumers (see full list in #479 issue)
   - Include #465, #466, #467, #468 as new consumers
   - Include all existing passes as migrated consumers
   - Include LSP enrichers as language consumers (parallel LSP falls out)
   - Include EmbeddingIndexer as streaming consumer
   - After merge: run ADR audit

### On #468 merge → queue:
- **Verification scan** of a repo with `.oh/extractors/` config present
- Confirm `Produces`/`Consumes` edges appear

### On #475 merge → queue:
- Integration test: crash mid-scan, verify sentinel prevents re-run

### On #477 merge → queue:
- Integration test: verify queries return results during rebuild (no zero-result window)

---

## Phase 2: Event Bus (after #478)

### On #479 merge → immediately queue:
1. **ADR audit agent** — all 5 constraints must pass
2. **#492 agent** — split `src/extract/` into consumer modules by detection hierarchy
   ```
   src/consumers/root/, extraction/, language/, framework/, completion/
   src/bus/mod.rs
   src/bootstrap/mod.rs
   ```
3. **Parallel LSP verification** — scan a multi-language repo, verify pyright + tsserver start concurrently (check logs)
4. **Performance test** — `time scan --repo . --full` < 120s

---

## Phase 3: Module Split (after #479)

### On #492 merge → queue:
1. **ADR audit agent** — constraint checks still pass after split
2. **Full test suite** — all tests pass with new module structure
3. **#454 agent** — singleton LanceDBPersist + EmbeddingIndexer consumers, multi-tenant store

---

## Phase 4: Multi-Tenant (after #492)

### On #454 merge → queue:
1. **ADR audit agent**
2. **Multi-repo test** — scan two repos simultaneously, verify both write to shared store with correct tenant_id
3. **Final performance test** — both repos scanned, time within budget

---

## Invariants (check after EVERY phase)

- `grep -r "kafka\|pubsub\|rabbitmq\|google.cloud" src/` returns 0 (no broker knowledge in core)
- `grep -r "PostExtractionRegistry\|EventBus" src/extract/` returns 0 (consumers don't know about registry)
- No `register()` calls inside `on_event()`
- `time scan --repo . --full` < 120s

---

## Remaining Issues to Queue (in order)

After #478: #479
After #479: #492, ADR audit
After #492: #454, ADR audit
After #468 (concurrent): verification scan
After #475 (concurrent): WAL integration test
After #477 (concurrent): append-only integration test
After #454: final ADR audit + full test suite + performance test
