---
id: validate-before-building
outcome: agent-alignment
severity: soft
statement: Validate the hypothesis before adding infrastructure — behavior change is the metric, not tool count
---

When tempted to add embeddings, LSP integration, multi-language parsers, or persistent indexes — first ask: does an agent behave differently with the tools we already have?

16 tools exist. The SLO (agent-scoping-accuracy) has zero observations. Adding tool #17 before measuring whether tools #1-16 change behavior is waste.

## Override Protocol
Override when the missing infrastructure is the direct blocker to validation (e.g., can't test on a Python project without Python tree-sitter support).
