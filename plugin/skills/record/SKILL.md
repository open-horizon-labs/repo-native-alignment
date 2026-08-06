---
name: record
description: Create or update repo-native outcomes, objectives, capabilities, signals, guardrails, metis, and ADRs. Use to preserve S&T lineage or record learning, measurements, constraints, and outcome status.
---

# Record Business Artifact

Write a structured markdown file to `.oh/` with YAML frontmatter. Use the templates below for each type.

## Arguments

`$ARGUMENTS` should be: `<type> <slug> [options]`, where type is `outcome`, `objective`, `capability`, `signal`, `guardrail`, `metis`, or `adr`.

Example: `/rna-mcp:record metis protocol-mismatch-hangs`.

## JIT References

For `outcome`, `objective`, or `capability`, or whenever the input includes S&T lineage,
load [references/open-horizons-outcome-family.md](references/open-horizons-outcome-family.md).
It contains the outcome-family templates and lineage rules.

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

### Outcome family

`outcome`, `objective`, and `capability` use `.oh/outcomes/<slug>.md`.
Load the outcome-family reference before creating or updating one.
Require a canonical parent outcome for objectives and capabilities.
Copy supplied S&T lineage exactly; candidate tactics are not selected work.

## Process

1. Parse `$ARGUMENTS` to determine type and slug.
2. Resolve the path. `outcome`, `objective`, and `capability` all use `.oh/outcomes/<slug>.md`.
3. For `objective` or `capability`, verify the parent `.oh/outcomes/<outcome-id>.md` exists. Stop and ask for the canonical parent rather than inventing one.
4. Check whether the target exists. Confirm before replacing metis/signal/guardrail prose; merge outcome-family frontmatter and body updates.
5. Read one existing artifact of the same type or outcome family for local format reference.
6. Copy supplied S&T lineage exactly: `s_and_t_step`, `parent_step`, `sufficiency_group`, `owner`, and `review_trigger`. A candidate tactic is not selected work.
7. Write the file using the Write tool.
8. Confirm: "Recorded <type> at `.oh/<subdir>/<slug>.md`".

## Slug Rules

- Use lowercase letters, numbers, and hyphens only.
- Reject path separators (`/`, `\`, `..`).
- Examples: `protocol-mismatch-hangs`, `agent-scoping-accuracy`.


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
- Prefer `cargo_tests` when a normal test can express the claim.
- Use built-in `audits` for code-shape constraints that are not honest test cases.
- Use direct executable references, not opaque evidence IDs.
- If validation does not exist, omit it and name the missing check in the ADR body or follow-up work.
- After creating or updating an ADR, run `/rna-mcp:adr-sync` to align frontmatter, compile/check output, and missing-check reporting.
