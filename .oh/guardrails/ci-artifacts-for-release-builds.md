---
id: ci-artifacts-for-release-builds
outcome: context-assembly
severity: hard
statement: Release builds and user-facing verification must use the successful GitHub Actions artifact for the target commit, not a local cargo install from source.
---

## Rationale

The CI artifact is what ships to users. Local source installs can differ from the release workflow through profile settings, environment, linker behavior, generated assets, or a dirty working tree. Verifying a locally built binary can therefore prove the wrong thing and leave the shipped artifact untested.

## What this means

- For release readiness, install or verify the GitHub Actions artifact produced by the successful workflow for the exact target commit.
- Use local `cargo check`, targeted tests, and debug builds only during development.
- Do not use `cargo install --path .` for MCP reloads, smoke checks, release validation, or anything described as a shipped/user build.
- If a local build was accidentally used, replace it with the CI artifact and re-run the observable verification before making a release decision.

## Evidence

A release-readiness check nearly validated a local `cargo install --path .` binary instead of the post-merge CI artifact. The stale instruction in `AGENTS.md` made that failure mode likely.
