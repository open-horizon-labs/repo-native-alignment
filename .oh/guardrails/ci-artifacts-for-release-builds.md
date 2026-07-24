---
id: ci-artifacts-for-release-builds
outcome: context-assembly
severity: hard
statement: Release builds and user-facing verification must use a successful GitHub Actions artifact whose Rust artifact inputs match the target commit; when neither Rust CI nor artifact inputs changed, reuse the existing artifact and do not dispatch or wait for redundant Rust CI.
---

## Rationale

The CI artifact is what ships to users. Local source installs can differ from the release workflow through profile settings, environment, linker behavior, generated assets, or a dirty working tree. Verifying a locally built binary can therefore prove the wrong thing and leave the shipped artifact untested.

Artifact identity is determined by its inputs, not by an unrelated commit SHA. Rebuilding the same Rust inputs after a Python harness, documentation, or post-build smoke-assertion change wastes time and can incorrectly turn redundant Rust CI into a delivery blocker.

## What this means

- First classify the diff from the successful artifact's source commit to the target commit along two boundaries:
  - Rust CI inputs include Rust source or tests, Cargo manifests/lockfiles, toolchain and `.cargo/` configuration, build scripts, embedded/generated assets, and release build/package logic.
  - Rust artifact inputs are the subset that can change shipped binary/package bytes. Rust test-only files and post-build smoke assertions are not artifact inputs.
- If any Rust artifact input changed—or artifact-input classification is genuinely uncertain—require a successful artifact built from the exact target commit.
- If Rust CI inputs changed but artifact inputs did not, run the relevant Rust checks but reuse the already-successful artifact.
- If neither Rust CI nor artifact inputs changed, reuse the already-successful artifact. Record its source commit and digest plus the diff evidence proving input equivalence. **Do not dispatch, rerun, or wait for Rust build, test, lint, audit, or artifact jobs.**
- Non-Rust changes still require exact-head checks for the surfaces they changed. A Python harness or post-build smoke assertion must be tested as such; it does not require a new Rust artifact.
- If the available workflow cannot separate relevant non-Rust checks from redundant Rust jobs, do not trigger the full workflow merely to obtain a new commit-bound artifact. Run a selective check path and record the CI-routing gap.
- Use local `cargo check`, targeted tests, and debug builds only during development.
- Do not use `cargo install --path .` for MCP reloads, smoke checks, release validation, or anything described as a shipped/user build.
- If a local build was accidentally used, replace it with the CI artifact and re-run the observable verification before making a release decision.

## Evidence

A release-readiness check nearly validated a local `cargo install --path .` binary instead of the post-merge CI artifact. The stale instruction in `AGENTS.md` made that failure mode likely.

Issue #830 later waited for repeated Rust artifacts even though intervening commits changed only the Python selector harness and post-build smoke assertions. Those builds could not change the Rust binary; exact-head delivery checks and Rust artifact provenance needed to be treated as separate gates.
