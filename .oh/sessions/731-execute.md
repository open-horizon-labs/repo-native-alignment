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

- [x] CI installs exact locked `cargo-audit` 0.22.2.
- [x] The vulnerability fixture exits 1 and names the advisory/package/version.
- [x] Current warning decisions are explicit, complete, unexpired, and printed.
- [x] Self-tests reject unknown, stale, and expired warning policy records.
- [x] The 829-dependency current lockfile passes with 0 vulnerabilities and the
  three exact warning records.
- [x] README documents the fixture and live local reproduction commands.

## Verification evidence

- `python3 .github/scripts/check-rustsec-policy.py --self-test` — pass.
- `python3 .github/scripts/check-rustsec-policy.py --live` — pass against
  RustSec database commit `9f3e138091487e69144f536d36976e427a7a3307`.
- Vulnerability fixture command — expected exit 1 with
  `RUSTSEC-2099-9999: fixture-vulnerable 1.0.0`.
- `cargo tree --all-features -i` evidence captured exact reverse paths for
  `lru@0.12.5`, `number_prefix@0.4.0`, and `paste@1.0.15`.
- Ruby YAML parse of `.github/workflows/rust-main-merge.yml` — pass.
- `git diff --check` — pass.

## Stop / pivot triggers

- Pivot if cargo-audit's machine report cannot distinguish vulnerability and
  informational findings without lossy text parsing.
- Pivot if a policy exception would need to cover an advisory version range
  rather than the exact checked-in finding.
