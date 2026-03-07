---
id: rna-live-verify-protoc-preflight
outcome: agent-alignment
title: RNA tools verified live; protoc preflight needed for smooth local install
---

Verified RNA MCP end-to-end in-session per AGENTS.md workflow:

1. `oh_get_outcomes` and `oh_get_guardrails` returned expected business context.
2. `outcome_progress("agent-alignment")` returned structural join data (outcome, tagged commits, code symbols, markdown references).
3. Practical install friction persists at local bootstrap: `cargo install --path .` failed until `protoc` was installed (`brew install protobuf`).

Implication: install/init path should preflight for `protoc` before compile and emit explicit remediation commands.
