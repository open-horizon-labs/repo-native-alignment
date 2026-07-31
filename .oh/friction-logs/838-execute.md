---
title: Issue #838 resident query runtime friction
date: 2026-07-29
issue: 838
outcome: context-assembly
---

# Friction Log: #838 Resident Query Runtime

| Phase/Step | Tool | What happened | Workaround | Severity |
|---|---|---|---|---|
| Existing-generation API location | RNA exact source retrieval | RNA returned stale line ranges for `EmbeddingIndex::new`; the current worktree source at those ranges was unrelated. | Used one bounded `rg` over the two already-identified embedding modules to locate the exact implementation lines. | low |
| Request-path bodies and tests | RNA exact symbol search | RNA found the right symbols but the CLI projection did not return the function bodies needed to patch and test the request path. | Used bounded `sed`/`rg` only inside the RNA-identified files and line neighborhoods. | low |
| #839 integration audit | RNA exact search plus final branch diff | The resident-runtime branch timed root discovery but did not yet time #839's newly opt-in enrichment-ledger read, leaving one acceptance phase unmeasured. | Added one shared verbose-only ledger loader/timer for primary and external MCP repository paths and removed formatter-only graph hunks from the final diff. | medium |
