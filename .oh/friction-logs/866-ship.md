---
pr: 866
phase: ship
---

| Phase | Fallback | Reason | Status |
|---|---|---|---|
| Pre-flight RNA scan | `repo-native-alignment` CLI | The RNA CLI is not installed or exposed in this task context; the mandatory scan command returned `command not found`. | skipped |
| Step 1 code review | `rg`/shell | RNA search and graph traversal tools are not exposed; targeted fallback inspection was required after the failed scan gate. | skipped |
