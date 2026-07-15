---
id: changed-file-lsp-scope-must-filter-scheduler-and-gate
outcome: context-assembly
title: 'Changed-file LSP scope must filter the scheduler and gate'
---

A bounded enrichment plan is not bounded merely because discovery starts from changed files. The work-producing event and every downstream continuation must carry the planned stable-node identities; otherwise a root-level fallback silently expands a one-file diff to the repository.

Filtering the scheduler alone is also unsafe. Any completion gate that counts languages or work independently must apply the identical predicate, or it can wait for work that was deliberately excluded. The regression should prove both halves together while also proving the full cached graph survives in-memory finalization and persistence replaces only scoped LSP rows.

Git discovery and storage must stay bounded too. Discovery should compare `HEAD` directly with the current worktree so staged changes later restored in the working copy do not produce phantom work. Persistence should remove prior LSP edges touching the planned nodes and apply only the refreshed scoped node/edge delta through the incremental seam, leaving unrelated rows untouched.

Scoped execution and global readiness are separate claims. A changed/root run must record scoped coverage even when every planned item succeeds, suppress repo continuation, and leave dead-code readiness partial. The honest boundary is: useful review context, not repo-wide caller/reference completeness.
