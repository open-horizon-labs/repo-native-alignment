---
id: commit-tagging-convention
outcome: agent-alignment
title: Commit Tagging Convention for Outcome Tracking
---

## Convention

Tag commit messages with `[outcome:X]` to link commits to outcomes.

Example:
```
Fix auth flow for SSO integration [outcome:agent-alignment]
```

## How it works

The `outcome_progress` tool parses `[outcome:{id}]` from commit messages to find commits that explicitly serve an outcome. This is combined with the outcome's `files:` patterns for a complete picture.

## When to tag

- Tag when a commit directly advances a declared outcome
- Don't tag routine maintenance, dependency updates, or refactoring unless they serve a specific outcome
- Multiple tags are fine: `[outcome:agent-alignment] [outcome:api-reliability]`

## Enforcement

This is a convention, not enforced by tooling. The value comes from making the link explicit for `outcome_progress` queries.
