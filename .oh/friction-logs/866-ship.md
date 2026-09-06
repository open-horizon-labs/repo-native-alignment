---
pr: 866
phase: ship
---

| Phase | Fallback | Reason | Status |
|---|---|---|---|
| Pre-flight RNA scan | `repo-native-alignment` CLI | The RNA CLI is not installed or exposed in this task context; the mandatory scan command returned `command not found`. | skipped |
| Step 1 code review | `rg`/shell | RNA search and graph traversal tools are not exposed; targeted fallback inspection was required after the failed scan gate. | skipped |
| September 6 remediation | RNA CLI search and node bodies | Used worktree debug RNA with 45,786 indexed symbols; LSP impact unavailable. Node hydration returned pre-precision source, confirmed by failed context patch. | stale |
| September 6 diagnostic edit | Targeted `sed` of `runtime_diagnostic` | Read current function after RNA hydration proved stale; no broad source fallback. | skipped |
| September 6 build configuration | `rg` of Cargo dependency/feature declarations | Inspect dependency wiring for direct production ORT encoder. | skipped |
| September 6 regression/runtime investigation | Targeted `rg`/`sed` of newly edited tests, report helper, pinned dependencies and build scripts | Indexed bodies were stale; all additional source-navigation fallbacks during remediation are recorded here. Later switched to RNA CLI `search --file --line --end-line`, which returns current filesystem spans. | skipped |
