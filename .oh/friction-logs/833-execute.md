---
type: friction-log
issue: 833
status: active
date: 2026-07-31
---

# Issue #833 execution friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-31 | RNA exact search → source span | minor | The installed CLI initially returned an older extracted snapshot after the branch-local implementation changed. | Refreshed the extract-only index before relying on symbol locations; no LSP or embedding rebuild was needed. | Make dirty-source freshness explicit in every search response and offer an opt-in refresh. |
| 2026-07-31 | RNA impact query → bounded `rg` fallback | minor | RNA found `node_lsp_position` but the extract-only graph had no caller edges, so the impact query returned no dependents. | Used one bounded `rg` call to identify the two call sites, then returned to RNA source spans for both bodies. | Surface syntax-level same-file references when LSP call/reference edges are unavailable. |
| 2026-07-31 | RNA struct-literal search → bounded diff/source fallback | minor | RNA located the affected `LspWorkItemSeed` and record literals but did not render enough parent context for coordinated edits. | Used only the current five-file diff and RNA-located source spans; no broad repository read was needed. | Let batch retrieval include the containing function for struct literals. |
| 2026-07-31 | RNA stale test symbol → bounded `awk` fallback | minor | The branch-local extract-only snapshot did not yet contain the newly added cross-file recovery test needed as an insertion anchor. | Read only that one test body, added the unborn-Git regression, and did not search unrelated source. | Support querying dirty symbols added after the last extract-only snapshot without a full rescan. |
| 2026-07-31 | RNA review search → bounded Git source spans | minor | RNA located the final recovery and readiness functions, but the extract-only graph could not deliver full function bodies or caller/reference coverage for the exact merged diff. | Reviewed only the RNA-located source spans and branch diff needed for the ship gates; no unrelated repository traversal was used. | Add bounded full-symbol rendering and syntax-level callers to degraded review-readiness output. |
| 2026-07-31 | Rustfmt exact-file check | minor | The current formatter reports broad pre-existing drift in the large parent modules even when checking the three touched Rust files. | Applied only the formatter's changes inside the new/modified hunks and left unrelated source untouched; strict Clippy and exact-head tests remain authoritative. | Add a changed-hunk formatting gate or normalize the repository in a dedicated change. |
