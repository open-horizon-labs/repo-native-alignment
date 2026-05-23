---
id: 2026-05-23-p0-release-blockers
outcome: context-assembly
severity: mixed
---

# P0 release blockers friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-05-23 | RNA MCP `search(query="Fix Pass 1 scheduling dead-code readiness delivery #677 queue changed scope", include_artifacts=true)` | minor | Search timed out after 60s while investigating #677 prior art. | Had to rely on GitHub issue/PR context and local files for the #677 split decision instead of a clean RNA artifact query. | Keep #677 as a dedicated control-plane issue; avoid overloading the scan/release blocker PR with incomplete LSP architecture work. |
| 2026-05-23 | `cargo fmt` | minor | Running formatter after a surgical Rust change reformatted unrelated files that were not part of the task. | Created noisy working-tree drift that had to be reverted before shipping. | Use targeted manual formatting or `rustfmt --check` first on this repo when minimizing diffs is required. |
