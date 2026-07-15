# Issue #731 — Deterministic RustSec gate

## Aim

Make the checked-in Rust dependency graph produce a release-blocking security
decision in CI, while keeping every non-vulnerability RustSec finding explicit,
owned, and time-bounded.

## Problem statement

The repository has a recently audited `Cargo.lock`, but Rust CI does not run
RustSec. Dependabot state therefore cannot prove that the authoritative lockfile
is free of vulnerability-class advisories. Running bare `cargo audit` would fix
that gap only partially: informational findings would remain visible but would
not have a checked-in decision contract.

## Solution space

### A. Bare `cargo audit`

Pin `cargo-audit` and run it in CI. This correctly fails vulnerability-class
findings, but it does not require ownership, expiry, dependency-path evidence,
or a removal issue for warning-class findings. Rejected as incomplete.

### B. Put advisory IDs in `.cargo/audit.toml`

This is mechanically narrow, but an ignore list cannot express or validate the
review metadata required by the issue, and it risks turning temporary decisions
into silent permanent suppression. Rejected.

### C. Pinned scanner plus checked-in policy validator

Install an exact, locked `cargo-audit` version; capture its JSON report; and
validate that report against a checked-in warning policy. The validator fails
on every vulnerability, unknown warning, expired exception, incomplete policy
record, or stale exception. Deterministic JSON fixtures prove both the failing
and passing decisions without depending on the live advisory database. Selected.

### D. Replace the dependency-policy stack with `cargo-deny`

This could cover more supply-chain policy, but it expands the problem beyond
RustSec release signaling and would still need the same project-specific
warning metadata. Rejected for this issue.

## Selected plan

1. Add a human-readable RustSec policy containing exact advisory IDs,
   dependency paths, rationale, owner, removal issue, review trigger, and expiry.
2. Add a standard-library-only validator for `cargo audit --json` reports.
3. Add deterministic pass/fail fixtures and a self-test command.
4. Add a pinned CI job, triggered by manifests, lockfile, toolchain, workflows,
   scripts, fixtures, or policy changes.
5. Document the exact local reproduction command.

## Acceptance evidence

- [ ] CI installs an exact locked cargo-audit release.
- [ ] Vulnerability fixtures and live vulnerability reports fail.
- [ ] Current warning decisions are explicit, complete, unexpired, and visible.
- [ ] Unknown, stale, or expired warning policy records fail.
- [ ] The current lockfile passes the declared policy.
- [ ] Local reproduction is documented.

## Stop / pivot triggers

- Pivot if cargo-audit's machine report cannot distinguish vulnerability and
  informational findings without lossy text parsing.
- Pivot if a policy exception would need to cover an advisory version range
  rather than the exact checked-in finding.
