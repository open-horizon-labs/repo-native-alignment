---
id: ship-steps-visible-on-pr
outcome: agent-alignment
severity: soft
statement: Ship pipeline steps must post findings as PR comments. A quality gate that only exists in a session file is no gate at all.
---

## Rationale

The ship pipeline's value is that each step's findings are posted as PR comments where they are visible to reviewers, auditable in GitHub's permanent record, and independent of the agent's context window. Session file notes are private scratchpad; PR comments are public record.

## Override Protocol

Skip only for draft PRs where the ship pipeline hasn't been formally invoked yet.

## Evidence

PRs #148, #149, #150 merged with no visible quality gate — dev-pipeline agent simulated ship steps internally instead of spawning the ship agent.

Source: metis/ship-steps-must-be-visible.md
