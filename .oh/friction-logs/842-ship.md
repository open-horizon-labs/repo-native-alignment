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
| External delivery verification | Real MCP SDK dependency discovery | low | RNA indexes repository code and artifacts, not temporary downloaded GitHub artifacts or the SDK installed under `/tmp`. | Used one bounded exact-path search in the temporary verification directory, then exercised the exact CI binary through an SDK stdio client. |
