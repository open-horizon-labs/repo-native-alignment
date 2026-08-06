# Open Horizons Outcome-Family Records

Load this reference for `outcome`, `objective`, or `capability`, or when input includes S&T lineage.

## Outcome

Write to `.oh/outcomes/<slug>.md`. If the file exists, merge updates without discarding body context.

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

## Objective or Capability

Store both in `.oh/outcomes/<slug>.md` so RNA discovery and `outcome_progress` can find them.
The `kind` field distinguishes the artifact type.
Require a canonical parent outcome and reference it with the existing `outcome` frontmatter key.

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

Do not encode a tactic as a capability to make it durable. A capability is an ability that will exist; a tactic is the chosen way to establish or use it.

## Lineage Gates

- Preserve `s_and_t_step`, `parent_step`, `sufficiency_group`, `owner`, and `review_trigger` exactly as supplied.
- Do not infer that a candidate tactic has been selected.
- Stop when an objective or capability lacks a canonical parent outcome; ask for the parent rather than inventing one.
