---
name: record
description: Record a business artifact (.oh/ metis, signal, guardrail, or outcome update). Use when capturing learnings, measurements, constraints, or updating outcome status.
---

# Record Business Artifact

Write a structured markdown file to `.oh/` with YAML frontmatter. Use the templates below for each type.

## Arguments

`$ARGUMENTS` should be: `<type> <slug> [options]`

Example: `/rna-mcp:record metis protocol-mismatch-hangs`

## Templates

### Metis (learning)

Write to `.oh/metis/<slug>.md`:

```markdown
---
id: <slug>
title: "<title>"
outcome: <related-outcome-id>
---

<body — what was learned and why it matters>
```

### Signal (measurement)

Write to `.oh/signals/<slug>.md`:

```markdown
---
id: <slug>
outcome: <related-outcome-id>
type: slo|metric|qualitative
threshold: "<measurable threshold>"
---

<body — what this measures and how>
```

### Guardrail (constraint)

Write to `.oh/guardrails/<slug>.md`:

```markdown
---
id: <slug>
severity: candidate|soft|hard
statement: "<one-line constraint>"
outcome: <related-outcome-id>
---

<body — rationale for this constraint>
```

### Outcome (update existing)

Edit the existing file at `.oh/outcomes/<slug>.md` — update `status`, `mechanism`, or `files` in the frontmatter.

## Process

1. Parse `$ARGUMENTS` to determine type and slug
2. Check if the file already exists — if so, confirm before overwriting (metis/signal/guardrail) or merge updates (outcome)
3. Read one existing artifact of the same type for frontmatter format reference
4. Write the file using the Write tool
5. Confirm: "Recorded <type> at `.oh/<subdir>/<slug>.md`"

## Slug Rules

- Lowercase, alphanumeric + hyphens only
- No path separators (`/`, `\`, `..`)
- Example: `protocol-mismatch-hangs`, `agent-scoping-accuracy`


## ADRs (architecture decisions)

When recording an ADR, write to `docs/ADRs/<NNN>-<slug>.md` with YAML frontmatter plus markdown body. Keep the ADR prose canonical, and make executable validation declarations match `plugin/skills/adr-sync/SKILL.md`.

### ADR template

```markdown
---
id: <NNN>-<slug>
status: proposed|implementing|implemented|superseded
validate:
  cargo_tests:
    - <exact cargo test name from `cargo test -- --list`>
  audits:
    - <exact built-in audit name if the claim is structural rather than behavioral>
  smoke:
    - <fixture path if needed>
  scripts:
    - <exact script path only if a normal test or audit cannot honestly express the claim>
---

# <Decision Title>

## Context
<why this decision exists>

## Decision
<what was decided>

## Consequences
<trade-offs and follow-on effects>
```

### ADR rules
- Prefer `cargo_tests` over every other check type whenever the claim can be expressed as a normal test
- Use built-in `audits` for code-shape constraints that are not honest test cases
- Use direct executable references only — no opaque evidence IDs
- If a validation does not exist yet, leave it out and call out the missing check explicitly in the ADR body or follow-up work
- After creating or updating an ADR, run `/rna-mcp:adr-sync` so frontmatter, compile/check output, and missing-check reporting stay aligned