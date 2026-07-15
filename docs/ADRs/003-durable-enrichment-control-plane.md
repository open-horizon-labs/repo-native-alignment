---
id: 003-durable-enrichment-control-plane
status: implementing
validate:
  cargo_tests:
    - server::enrichment_jobs::tests::job_transitions_are_persisted
    - server::enrichment_jobs::tests::overlapping_same_key_joins_active_job
    - server::enrichment_jobs::tests::new_job_supersedes_stale_non_terminal_job_from_store
    - server::enrichment_jobs::tests::scan_enrichment_options_preserve_structured_modes
    - server::tests::test_foreground_pipeline_incremental_on_second_run
    - server::tests::test_schema_version_bump_forces_full_rebuild_on_incremental_path
    - extract::lsp::work_items::tests::mixed_work_item_state_round_trips_and_reconstructs_snapshot
    - extract::lsp::work_items::tests::concurrent_ledgers_merge_without_losing_jobs
    - extract::lsp::work_items::tests::interrupted_jobs_are_bounded_separately_from_terminal_history
    - extract::lsp::work_items::tests::job_ids_are_unique_and_process_scoped_across_processes
    - extract::lsp::work_items::tests::concurrent_process_does_not_reclaim_initializing_owner_file
    - server::operation_report::tests::persisted_lsp_work_queue_is_attached_and_rendered_for_list_roots
    - extract::lsp::work_items::tests::interrupted_queue_resumes_only_eligible_items_once
    - extract::lsp::work_items::tests::schema_v1_completed_items_are_replayed_conservatively
    - extract::lsp::work_items::tests::changed_node_input_replays_instead_of_carrying_stale_output
    - extract::lsp::passes::tests::recovered_pass1_edges_are_applied_idempotently
    - extract::lsp::passes::tests::interrupt_restart_executor_fixture_invokes_only_retryable_items_once
    - extract::lsp::passes::tests::exhausted_recovery_fails_pass1_closed_without_invocation
---

# Durable Enrichment Control Plane
**Date:** 2026-04-30

## Context

RNA extraction is fast enough to make a graph queryable, but LSP and embedding enrichment are slower, optional, and capability-specific. Before this decision, enrichment state was scattered across in-memory status fields, background task handles, and coarse sentinels such as `lsp_completed.json`.

That made several operational states ambiguous:

- a repo could be extract-ready while call/reference or embedding enrichment was still unavailable;
- foreground and background enrichment could emit indistinguishable progress;
- a no-op or scoped scan could accidentally look like a full capability refresh;
- after restart, agents could not inspect recent enrichment work through RNA/MCP surfaces;
- skipped LSP enrichment could leave stale LSP sentinel state behind unless every path remembered to clear it.

The capability-readiness model needs a durable control-plane primitive that records enrichment work as jobs, not just a boolean status bit.

## Decision

Add a repo-native enrichment job ledger and structured scan enrichment options.

The ledger is persisted at `.oh/.cache/enrichment_jobs.json` and records:

- job id;
- repo/root/scope;
- capability (`extracted_graph`, `call_references`, `embeddings`);
- trigger (`startup`, `foreground_scan`, `background_scan`, `incremental_refresh`, `explicit`);
- lifecycle state (`queued`, `running`, `persisting`, `completed`, `failed`, `cancelled`, `superseded`);
- counters and failure details;
- event history.

`RnaHandler` owns a shared `EnrichmentJobLedger`. Foreground and background LSP/embedding paths begin jobs before work, update progress while running, mark persisting before cache writes, and complete/fail explicitly. In-process overlapping work for the same repo/capability/scope joins the active job. Persisted non-terminal jobs from a previous process are superseded by the next job for that same key.

Pass 1 call/reference enrichment also persists versioned per-work-item records at `.oh/.cache/lsp_pass1_work_items.json`. Each record keeps its job, repo/root/file/node identity, requested operations, lifecycle state, attempt and phase history, timestamps, and last error. Queue snapshots are reconstructed from these records and attached to `OperationReport`; `list_roots` therefore delivers the same pending, in-flight, terminal, phase-count, and oldest-work view through MCP. Writes are atomic, phase writes are throttled, terminal state is flushed before the pass returns, and only a bounded number of jobs is retained.

Interrupted Pass 1 jobs recover at the same ledger seam. Records match by stable node identity, source-input fingerprint, and requested operations. Completed output and skipped state carry forward without another LSP request only while that input identity matches; schema-v1 and changed-input records replay conservatively. Pending, stale in-flight, and failed items retry up to three attempts; exhausted items remain terminal, fail readiness closed, and surface bounded actionable diagnostics. Recovered edges and virtual nodes are applied by stable identity so replay remains idempotent. New scans start a fresh job once the prior queue has no unfinished eligible work.

Scan callers now pass structured `ScanEnrichmentOptions` instead of relying on implicit behavior:

- `all()` runs LSP and embeddings;
- `extract_only()` skips both;
- `without_lsp()` skips call/reference enrichment only;
- `without_embeddings()` skips semantic-index enrichment only.

The CLI exposes these controls as `scan --extract-only`, `scan --no-lsp`, and `scan --no-embed`.

Verbose search/MCP search context includes recent enrichment jobs so agents can see capability work and failure state through the normal query surface.

## Why not a single readiness boolean?

A boolean collapses distinct capabilities into one lossy state. Extracted graph readiness, semantic embedding readiness, and call/reference readiness fail independently and recover independently. The control plane must preserve capability, trigger, scope, and lifecycle details or agents will keep guessing which prerequisite is missing.

## Why not rely on sentinel files only?

Sentinels are useful completion markers, but they cannot represent active work, failures, supersession after restart, event history, or concurrent foreground/background coordination. They remain cache validity hints; the job ledger records operational intent and progress.

## Why not add an external scheduler?

RNA is repo-native and lightweight. A separate daemon, database, or queue would violate the local-first operating model and add setup requirements before agents can query a repo. A JSON ledger under `.oh/.cache` is sufficient for the current single-process control-plane needs and keeps scanner behavior valid for non-git directories.

## Consequences

- Agents can inspect recent enrichment history without logs or shell access.
- Scoped scans can deliberately avoid LSP/embedding work without pretending those capabilities are refreshed.
- Incremental scans that skip LSP must clear stale LSP sentinels after a successful persist.
- The ledger is not a distributed lock. It coordinates active jobs inside one process and supersedes stale persisted state after restart.
- Pass 1 work-item persistence makes queue state inspectable after restart; recovery/retry policy is a separate decision and must not silently replay completed items.
- The job file is cache/control-plane state, not source truth; the graph and LanceDB cache remain the query substrate.

## Validation Notes

Current executable coverage proves:

- job lifecycle transitions persist to disk;
- overlapping in-process jobs join the active job;
- stale persisted running jobs are superseded after restart;
- scan options preserve independent LSP and embedding capability choices;
- foreground incremental and schema-version rebuild paths still work with explicit enrichment options.
- mixed pending, in-flight, completed, failed, and skipped Pass 1 records round-trip and reconstruct the same bounded queue snapshot;
- persisted Pass 1 snapshots are attached to OperationReport history and rendered through the `list_roots` MCP surface.

Manual CLI verification also covered:

- `scan --extract-only` skips LSP and embeddings and does not write an LSP sentinel;
- `scan --full --no-embed` records completed call-reference enrichment jobs;
- verbose search renders recent enrichment jobs;
- a full LSP scan followed by extract-only incremental scan clears stale LSP sentinel state.

## References

- Issue #664
- Issue #730
- PR #665
- ADR-001 (event bus extraction pipeline)
- ADR-002 (ArcSwap graph concurrency)
