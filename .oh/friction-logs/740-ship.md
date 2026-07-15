# Friction Log — PR #740

| Step | Friction | Impact | Follow-up |
|---|---|---|---|
| Execute | RNA search indexes `BusOptions` and `LspPipelineInput` definitions but not their struct-literal construction sites. After `cargo check --lib` identified nine exact missing-field lines, targeted source-line reads were needed to patch the construction sites. | Small, bounded fallback after RNA and compiler diagnosis; no broad grep. | Consider indexing Rust struct literals or connecting missing-field compiler diagnostics to construction-site graph nodes. |
| Execute | Local Rust 1.97 Clippy with `-D warnings` reports roughly 80 pre-existing warnings across unchanged tests and modules, while the exact parent head's pinned CI lint is green. Two warnings in the changed gate logic were still fixed immediately. | Local all-target Clippy cannot serve as a clean repository gate with this newer toolchain. | Use the repository-pinned exact-head CI lint as authoritative; keep local review scoped to warnings in changed code. |
