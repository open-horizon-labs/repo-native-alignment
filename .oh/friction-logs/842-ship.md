---
artifact_type: friction-log
issue: 839
pr: 842
updated: 2026-07-30
---
# Friction log: PR #842 ship

| Phase | Tool path | Severity | Friction | Impact / response |
|---|---|---:|---|---|
| Final-diff review | Exact changed helper search through the installed RNA CLI | medium | One RNA query returned the pre-fix `persist_report` body and omitted the newly committed `report_temp_paths` helper even while reporting the current scan time. | Diagnosed the index/source disagreement explicitly, then used bounded `git grep` and exact line-range reads for only the changed publication functions. Subsequent RNA queries returned the current symbols and supplied impact context. |
| Reviewer-remediation review | RNA exact-symbol search, then bounded source fallback | low | RNA found the three persistence concurrency tests and their current locations, but compact search does not return full test bodies needed to reconcile old assertions with the new mandatory repo lock. | Used two bounded line-range reads after the RNA query, then rewrote the tests to state and assert the new serialization contract. |
| Lock-callsite dissent | RNA impact/exact search, then bounded `rg` and line-range fallback | medium | Without persisted LSP call edges, RNA found the report-builder chain but omitted its two async call sites, which mattered when checking whether a blocking file lock could stall the runtime. | Used one bounded symbol-name search and two narrow reads; confirmed both paths run on multi-thread runtimes or outside a competing mutation, while graph-writer lock acquisition itself is isolated with `spawn_blocking`. |
| External delivery verification | Real MCP SDK dependency discovery | low | RNA indexes repository code and artifacts, not temporary downloaded GitHub artifacts or the SDK installed under `/tmp`. | Used one bounded exact-path search in the temporary verification directory, then exercised the exact CI binary through an SDK stdio client. |
