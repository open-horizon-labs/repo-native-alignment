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
