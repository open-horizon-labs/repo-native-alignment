---
issue: https://github.com/open-horizon-labs/repo-native-alignment/issues/732
outcome: context-assembly
phase: execute
status: complete
updated: 2026-07-15
---

# Issue 732: Bounded changed-file call/reference enrichment

**Issue:** #732
**Outcome:** context-assembly
**Status:** implemented; ship gate pending

## Aim

An agent preparing a review can ask RNA to enrich the files it changed, see exactly what was scheduled and omitted, and get useful partial graph context without accidentally paying for or claiming repo-wide coverage.

## Problem

`EnrichmentScope::ChangedFiles` exists, but explicit call/reference enrichment rejects it. The event-bus pipeline can already limit work by root, while Pass 1 records durable per-node work items, but there is no planner that maps a changed-file set to those node work items. Passing the primary root as dirty would schedule every eligible node in that root and would mislabel repo-scale fanout as changed scope.

The implementation must also preserve two truths:

- the complete cached graph still flows through post-passes and in-memory finalization, while scoped incremental persistence cannot erase unrelated graph state;
- scoped coverage remains partial for global capabilities such as dead-code, even when every planned changed node completes.

## Solution space

### A. Reuse root scoping

Treat the primary root as dirty and run the current pipeline unchanged.

Rejected: a one-file diff can schedule the entire repository. It violates the acceptance criterion and the stop/pivot trigger.

### B. Build a separate changed-file LSP executor

Construct and execute work items outside the event bus.

Rejected: this duplicates operation selection, queue durability, restart handling, diagnostics, and persistence. It would create a second control plane beside #730 and #733.

### C. Plan changed nodes, then filter the existing event bus

Discover a deterministic git working-tree change set, map present files to eligible cached nodes and LSP operations, reject unusable input before opening an enrichment job, and pass the planned stable-node IDs through the existing event bus and durable Pass 1 queue.

Selected: it is the smallest design that makes bounded scope explicit while reusing the durable execution path.

## Selected design

### Planner inputs and provenance

Add a pure changed-file planner with explicit inputs:

- repository path and primary root slug;
- git change records for the net `HEAD -> current worktree` state, including old/new paths and status;
- provenance containing the resolved HEAD object ID and the `working-tree` target;
- the cached graph nodes from which work can be scheduled;
- an explicit maximum planned-node and operation budget.

Git discovery enables rename detection on one direct tree-to-worktree diff. Non-git repositories, bare repositories, unreadable HEAD/provenance, an empty usable change set, or an over-budget plan fail before `begin_job`, with help to use `--scope root --root <slug>` or `--scope repo`.

### Deterministic mapping

Normalize repo-relative paths and classify changes in sorted maps/sets:

- added/modified/copied/type-changed files map by their current path;
- renamed files report `old -> new` and map only the new path;
- deleted files are reported but cannot schedule nodes;
- present files with no eligible cached nodes are reported as unmapped;
- paths outside the repository are rejected.

Eligible nodes are the node kinds supported by Pass 1 and have a non-empty language. Requested operations use the same operation-selection function as Pass 1:

- function: references and/or call hierarchy according to server capability;
- trait: implementations;
- struct, enum, type alias, and const: references;
- supported `Other` nodes: document links.

The plan owns a sorted set of stable node IDs and a bounded operation count. No transitive or graph-neighbor expansion is performed.

### Execution seam

Extend the event-bus options with an optional planned-node ID set. Apply the identical predicate in both `LanguageAccumulatorConsumer` and `AllEnrichmentsGate`, so only planned nodes produce `LanguageDetected` events and the gate expects exactly those languages. Continue sending the full cached node/edge set through `RootExtracted` and the finalizer.

`run_foreground_lsp_and_persist` accepts the optional filter and still opens the normal durable enrichment job. For changed scope, planning and all prerequisite checks happen first, progress output renders provenance, scheduled counts, and deterministic diagnostics, then the planned node set is executed through the ordinary Pass 1 work-item ledger.

Before execution, cached LSP edges touching planned nodes are removed from the pipeline input so stale scoped relationships cannot survive a refresh. Persistence deletes those prior edge IDs and upserts only refreshed scoped LSP edges and their virtual nodes through the incremental LanceDB seam; unrelated graph rows are not rewritten.

Changed scope always suppresses background or run-to-completion repo continuation. The resulting LSP readiness is recorded with scoped coverage, never repo-wide completeness or a repo-wide sentinel.

## Acceptance criteria

- [x] Planner inputs name repo/root, changed files, limits, and git base/ref provenance.
- [x] One changed file schedules only eligible nodes in that file and their supported operations.
- [x] Execution uses the #730 durable Pass 1 work-item ledger.
- [x] Missing provenance/mapping prerequisites reject before a job record is created and name root/repo alternatives.
- [x] Scoped completion remains partial for global dead-code and explicit for review-readiness.
- [x] Deleted, renamed, unmapped, non-git, and out-of-repo cases have deterministic diagnostics.
- [x] Changed scope cannot start repo-wide continuation.
- [x] Changed scope persists only its LSP delta and removes stale scoped edges without rewriting unrelated rows.

## Implementation evidence

- Added a git-backed pure planner for net `HEAD` → current-worktree changes with rename detection, repo/root provenance, deterministic diagnostics, and 4,096-node / 12,288-operation hard bounds.
- Reused Pass 1's operation selector and delivered planned stable-node IDs through `BusOptions` to both `LanguageAccumulatorConsumer` and `AllEnrichmentsGate`.
- Kept the complete cached graph in the event payload and in-memory finalization while replacing only planned-node LSP rows through incremental persistence.
- Planned changed scope before `run_foreground_lsp_and_persist` opens the durable enrichment job and disabled every repo continuation mode for changed scope.
- Changed non-repo completion to `set_complete_scoped`, preserving partial global dead-code readiness and explicit review-readiness context.
- Added the one-file fanout oracle, real git provenance, staged-then-restored, deterministic rename/delete/unmapped, budget, non-git, scheduler/gate agreement, scoped persistence, full-graph preservation, and readiness regressions.
- Local verification after review fixes: `cargo +1.97.0 check --lib`; planner tests (7 passed); enrichment tests (9 passed); event-bus filter and scoped-readiness regressions (1 passed each); ADR validation (4/4 passed); `git diff --check`; targeted `rustfmt --check`. Exact-head full CI remains the final ship gate.

## Verification plan

1. Pure planner tests: one changed file among unrelated files; operation count; deterministic rename, deletion, and unmapped diagnostics; non-git and over-budget failures.
2. Event-bus tests: the language accumulator and enrichment gate apply the same node filter, emit no unrelated node/language, and complete without a mismatched count.
3. Executor tests: invalid changed plans fail before job creation; changed scope ignores requested background/run-to-completion continuation.
4. Readiness regression: successful scoped work cannot satisfy global dead-code readiness.
5. Adversarial oracle: a one-file diff fails any implementation that unions all graph nodes, silently falls back to repo scope, or emits unrelated stable node IDs.
6. `cargo check --lib`, targeted tests, full test suite, clippy/format, exact-head CI, release-artifact manual verification, and official MCP smoke.

## Stop/pivot trigger

Retain the current explicit rejection if normal one-file changes cannot be represented without effectively repo-wide scheduling, or if the event-bus filter cannot preserve the full cached graph while limiting durable work items.
