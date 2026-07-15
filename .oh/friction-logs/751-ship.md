# PR #751 ship friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-15 | RNA MCP `repo_map` / `search` / `outcome_progress` | skipped | No RNA MCP tools were exposed in this issue agent session. | Used the installed RNA CLI with `--repo .` after a full scan for symbol, artifact, and graph navigation. | Ensure issue agents inherit the repo's RNA MCP server. |
| 2026-07-15 | RNA function-body navigation | skipped | RNA identified symbols, callers, and neighbors but does not render the implementation body needed for a targeted edit. | Used bounded `sed` ranges after RNA located the exact symbols. | Add source-snippet rendering for exact symbol results. |
| 2026-07-15 | GitButler branch management | skipped | `but status -fv` reported that the checkout was not on a `gitbutler/*` branch. | Used direct Git to rebase the existing issue branch onto the newly shipped dependency. | Make the draft-PR issue branches GitButler-manageable without renaming them. |
| 2026-07-15 | Repository-wide `cargo fmt --check` | minor | Existing unrelated files are not rustfmt-clean, producing a large unrelated diff report. | Formatted and verified only the changed Rust file; no unrelated formatting changes were made. | Restore repository-wide formatting separately. |
