---
title: PR 780 execute friction log
artifact_type: friction-log
date: 2026-07-16
outcomes:
  - context-assembly
---

# PR 780 execute friction log

| Time | Severity | Event | Evidence and response |
|---|---|---|---|
| 2026-07-16 | skipped | RNA CLI located the SWE-bench harness symbols and graph neighbors but did not deliver the Python function bodies required to extend the orchestrator safely. | Used bounded source reads only for `scripts/swebench_rna_one.py`, its tests, and its documentation after RNA search and neighbor traversal. |
| 2026-07-16 | degraded | RNA index reported no embeddings and degraded TypeScript LSP coverage. | Restricted exploration to exact structural results; do not claim complete impact coverage. The initial known state was 41,424 symbols and 3,959 partial edges; a later query observed 75,674 symbols and 7,918 partial edges after unrelated generated fixture caches appeared, so the changing count is not treated as issue evidence. |
