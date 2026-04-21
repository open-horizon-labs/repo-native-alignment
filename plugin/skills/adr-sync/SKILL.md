---
name: adr-sync
description: Backfill ADR frontmatter with direct executable validation refs, compile/check ADR validation manifests, and report missing real checks without inventing them.
---

# ADR Sync

Translate ADR prose into executable validation declarations and verify that ADRs are actually wired to real checks.

## Arguments

`$ARGUMENTS` may be a single ADR path or a directory. Default to `docs/ADRs/` when omitted.

Examples:
- `/rna-mcp:adr-sync docs/ADRs/001-event-bus-extraction-pipeline.md`
- `/rna-mcp:adr-sync docs/ADRs`

## Goal

Make ADRs mechanically checkable without introducing a second source of truth.

The ADR markdown stays canonical. This skill:
- adds or updates thin frontmatter validation declarations
- points them directly at real repo executables
- runs the repo-native ADR compile/check path
- reports gaps where no real executable exists yet

## Valid executable references

Prefer normal test-suite checks first:
- exact cargo test names as reported by `cargo test -- --list`
- exact built-in ADR audit names when the claim is a code-shape constraint
- exact smoke fixture paths only when you need a repo-native delivery scenario

Use exact script paths only as a fallback when a claim genuinely cannot be expressed as a normal test or built-in audit yet.

Do **not** invent opaque evidence IDs or a registry layer in the ADR source.
Do **not** fabricate validations that do not exist.

## Process

### 1. Inspect the ADR(s)
- Read the ADR markdown and current frontmatter
- Extract the concrete claims that should be checkable
- Look for existing validations already present in the repo:
  - exact `cargo test -- --list` names
  - built-in ADR audits via `repo-native-alignment adr audit <name>`
  - smoke fixtures under `.github/fixtures/` only when a repo-native delivery check is required
  - scripts under `.github/scripts/` or `scripts/` only if no honest test or audit exists yet

### 2. Add/update frontmatter only where mapping is honest
Expected shape:

```yaml
---
id: 001-event-bus-extraction-pipeline
status: implemented
validate:
  cargo_tests:
    - exact::cargo::test_name
  audits:
    - exact_audit_name
  smoke:
    - .github/fixtures/smoke
```

Rules:
- Prefer the thinnest schema that points at real checks
- Prefer `cargo_tests` first, then built-in `audits`, then `smoke`, then `scripts` as the last resort
- Preserve ADR prose; only add the executable seam
- If a claim lacks a real check, leave it out and report the gap explicitly

### 3. Compile or check manifests
Run the repo-native ADR command path when available:

```bash
repo-native-alignment adr compile --repo .
repo-native-alignment adr compile --repo . --check
repo-native-alignment adr validate --repo .
```

If the command is not available yet, report that as a blocker instead of simulating success.

### 4. Report missing real checks
For each ADR claim that should be executable but is not yet backed by a real check, report:
- the missing claim
- the missing executable type needed (prefer `cargo test`; use script only as a last resort)
- whether the ADR should remain partially validated for now

## Output

```markdown
## ADR Sync Result

### Updated ADRs
- `docs/ADRs/001-...md`: added `validate.cargo_tests`
- `docs/ADRs/002-...md`: added `validate.cargo_tests`

### Compile / Check
- `repo-native-alignment adr compile --repo .` → [pass/fail]
- `repo-native-alignment adr compile --repo . --check` → [pass/fail]
- `repo-native-alignment adr validate --repo .` → [pass/fail]

### Missing Real Checks
- ADR 002: claim "tool calls never block" still needs a dedicated regression test

### Notes
- No fake validations added
- No indirect evidence registry introduced
```

## What NOT to do
- Do not invent test names or scripts that do not exist
- Do not mark an ADR fully compliant if required checks are still missing
- Do not default to `.github/scripts` when a normal test is the honest representation
- Do not move execution details into prose when frontmatter can carry the direct ref cleanly
- Do not create a parallel evidence-ID abstraction in ADR source

## Success condition
The ADR now declares direct executable validation refs, compile/check passes, and any missing real checks are explicitly surfaced instead of papered over.
