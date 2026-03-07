---
id: setup-two-tier-verification
outcome: agent-alignment
title: Setup command enables deterministic bootstrap; real-client verify remains separate tier
---

Execution of Option C confirmed a pragmatic split:

- `setup` can reliably enforce deterministic preflight/install/config merge in-repo with low complexity.
- True end-to-end verification still needs a real MCP client handshake, which is not equivalent to binary `--help` checks.

Practical consequence: treat verification as two tiers:
1. **Tier 1 (implemented):** local binary/config sanity checks (`--help`, `.mcp.json` entry).
2. **Tier 2 (next):** real-client smoke call against configured server.

This keeps rollout lightweight while preserving the `test-with-real-mcp-client` guardrail as the next hardening step.
