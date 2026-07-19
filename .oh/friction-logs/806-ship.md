---
artifact_type: friction-log
issue: 786
pr: 806
updated: 2026-07-19
---
# Friction log: PR #806 ship

| Phase | Tool path | Severity | Friction | Impact / response |
|---|---|---:|---|---|
| Pre-flight | Required metis path → RNA artifact search | low | `.claude/agents/ship.md` names retired `.oh/metis/computed-but-not-delivered.md`. | RNA located the promoted hard guardrail at `.oh/guardrails/computed-but-not-delivered.md`; no review gap. |
| Scan gate | Documented empty RNA query | low | The CLI reports the live index but rejects the empty query used by the ship instructions. | Supported exact artifact/symbol queries confirmed 45,134 indexed symbols, so no full/model/LSP scan was started. |
| Review | Exact new symbols / impact | medium | Schema-v23 omits new #786 APIs and attached LSP call/reference coverage. | RNA located owning existing seams; bounded exact PR diff/source reads and compiler/full-test evidence were used without claiming complete caller coverage. |
| Review | Exact workflow/Python contract | low | RNA does not deliver the complete new YAML or multi-function unstaged Python contract. | Bounded exact-diff/script reads surfaced real bundle, timing, and delivery-probe defects. |
| TODO/comment audit | Diff-scoped predicate / GitHub comments | low | RNA cannot query only added TODO text or external inline review comments. | A bounded exact-head diff scan found no TODO/FIXME markers; GitHub APIs found two minor code-quality comments for remediation. |
| CI | Rust 1.97 lint and policy | low | Local compile/test passed while pinned CI found two newer Clippy suggestions and the new feature missing from declared RustSec reachability. | Applied the exact lint changes and declared reachability identical to the feature's `metal` parent. |
| Adversarial remediation | New combined-lineage and pointer-publication symbols | low | The live schema-v23 RNA index did not contain the newly added #786 helpers or their test callers. | Both fix agents confirmed the stale result with broader RNA CLI queries, then used bounded exact-file fallbacks for the two assigned seams only. |
