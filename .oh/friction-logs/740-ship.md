---
pr: 740
issue: https://github.com/open-horizon-labs/repo-native-alignment/issues/732
outcome: context-assembly
phase: ship
date: 2026-07-15
---

# Friction Log — PR #740

| Step | Friction | Impact | Follow-up |
|---|---|---|---|
| Execute | RNA search indexes `BusOptions` and `LspPipelineInput` definitions but not their struct-literal construction sites. After `cargo check --lib` identified nine exact missing-field lines, targeted source-line reads were needed to patch the construction sites. | Small, bounded fallback after RNA and compiler diagnosis; no broad grep. | Consider indexing Rust struct literals or connecting missing-field compiler diagnostics to construction-site graph nodes. |
| Execute | Local Rust 1.97 Clippy with `-D warnings` reports roughly 80 pre-existing warnings across unchanged tests and modules, while the exact parent head's pinned CI lint is green. Two warnings in the changed gate logic were still fixed immediately. | Local all-target Clippy cannot serve as a clean repository gate with this newer toolchain. | Use the repository-pinned exact-head CI lint as authoritative; keep local review scoped to warnings in changed code. |
| Review follow-up | RNA search could retrieve the existing enrichment executor but returned no result for new `changed_file_plan` helpers after broader and file-scoped retries. | Targeted source-range reads were required for the exact CodeRabbit-commented planner code. | Keep this as an index-freshness dogfood case; no broad source search was used. |
| Review follow-up | Full ADR validation transiently failed the cross-process job-ID fixture even though it had passed earlier. | The exact test passed immediately in isolation, and ADR-003 then passed its isolated validation gate. | Treat the child-process fixture as flaky unless exact-head CI reproduces it. |
