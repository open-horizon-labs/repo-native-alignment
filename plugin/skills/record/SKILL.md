---
name: record
description: Create or update repo-native outcomes, objectives, capabilities, signals, guardrails, metis, and ADRs. Use to preserve S&T lineage or record learning, measurements, constraints, and outcome status.
---

# Record Business Artifact

Write a structured markdown file to `.oh/` with YAML frontmatter. Use the templates below for each type.

## Arguments

`$ARGUMENTS` should be: `<type> <slug> [options]`, where type is `outcome`, `objective`, `capability`, `signal`, `guardrail`, `metis`, or `adr`.

Example: `/rna-mcp:record metis protocol-mismatch-hangs`.

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

### Outcome (create or update)

Write to `.oh/outcomes/<slug>.md`. If it exists, merge updates without discarding body context.

```markdown
---
id: <slug>
kind: outcome
status: proposed|active|paused|achieved|abandoned
s_and_t_step: <step-id-or-null>
owner: <role-or-null>
review_trigger: "<when to reassess>"
files: []
---

# <Title>

## Desired behavior change
<who does what differently>

## Mechanism
<causal hypothesis>

## Feedback
<observable signal and timeframe>
```

### Objective or capability (create or update)

Store objectives and capabilities in `.oh/outcomes/<slug>.md` so RNA discovery and `outcome_progress` can find them. The `kind` field distinguishes the artifact type. Require a canonical parent outcome, referenced by the existing `outcome` frontmatter key.

```markdown
---
id: <slug>
kind: objective|capability
status: proposed|active|paused|achieved|abandoned
outcome: <parent-outcome-id>
s_and_t_step: <step-id>
parent_step: <parent-step-id-or-root>
sufficiency_group: <group-id-or-null>
owner: <role-or-null>
review_trigger: "<when to reassess>"
files: []
---

# <Title>

## Statement
<objective to achieve or capability to establish>

## Why it matters
<necessity relative to the parent outcome>

## Enables
<downstream objective, capability, or tactic>

## Acceptance signal
<observable evidence this branch is ready or achieved>
```

Do not encode a tactic as a capability merely to make it durable. A capability describes an ability that will exist; a tactic describes the chosen way to establish or use it.

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
