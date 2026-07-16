---
date: 2026-07-15
outcome: agent-alignment
source_issue: 642
title: Session-scoped executable artifacts need explicit activation
---

# Recommendation: LIMITED-GO

RNA should support executable validation for an explicitly selected subset of
guardrails, but it should not make metis directly executable. The useful
primitive is an ephemeral, reviewable activation plan that points from a session
to canonical repo-native guardrails and their real validation references. Metis
may help a human choose a guardrail or may be promoted into one through the
existing curation workflow; it must not silently become an enforcement rule.

## Aim

Agents should verify the constraints relevant to the work they are doing instead
of either ignoring durable guidance or treating every accumulated learning as a
universal rule. The signal of success is that a session can name the constraints
it selected, run their real checks, and explain any failure without changing the
canonical artifact or applying unrelated guidance.

## Problem framing

The issue is not whether arbitrary markdown can launch commands. It is how RNA
can gain executable honesty while preserving the distinction between decisions,
constraints, and situated learning:

- ADRs describe repository decisions with status and proof obligations.
- Guardrails constrain behavior, but applicability ranges from universal to a
  particular workflow or subsystem.
- Metis records contextual learning. The existing
  `metis-curation-requires-human-judgment` guardrail explicitly prohibits
  indiscriminate application and automated promotion.

The binding constraints are repo-native canonical artifacts, direct references
to real checks, explicit human judgment for metis, and no hidden global policy
created from session context.

## Evidence

### ADR validation is reusable only below its policy layer

PR #641 established useful mechanics: canonical markdown declares thin direct
references to exact cargo tests, built-in audits, smoke fixtures, or scripts;
RNA compiles those declarations and reports missing or failing checks. The
`adr-sync` skill also requires honesty: no opaque evidence registry, no invented
checks, and no claim of full compliance when proof is missing.

Those reference types and runner/reporting mechanics are reusable. ADR status
gating is not: an implemented ADR is globally applicable, whereas an optional
guardrail needs an explicit activation decision. Generalizing the full ADR model
would erase that semantic difference.

### Existing curation already defines the metis boundary

The `distill` workflow lets an LLM surface and cluster metis, but a human decides
whether to keep, promote, compact, or dismiss it. Only an explicitly approved
promotion creates a guardrail. Directly attaching executable enforcement to
metis would bypass the judgment step and contradict the existing guardrail.

### An existing pattern demonstrates honest optional enforcement

`advisory-exceptions-need-a-failing-clock-and-live-graph` describes an optional,
time-bounded exception that becomes honest through executable checks of the live
dependency graph and expiry. This is a useful model: execution verifies the
declared scope and drift of a human decision; it does not autonomously decide
that the exception applies.

## Candidate solutions

| Option | Description | Assessment |
|---|---|---|
| Global executable fields on guardrails and metis | Run every declared check in every session | NO-GO: noisy, expensive, and violates contextual metis semantics |
| Copy ADR validation wholesale | Give optional artifacts ADR-like status gates | NO-GO: reuses mechanics but imports the wrong global policy model |
| Explicit session activation plan | Human or workflow selects guardrails; plan records exact artifact IDs and direct validation refs | LIMITED-GO: preserves selection and enables reproducible checks |
| Keep all artifacts advisory | Make no executable extension | Safe, but leaves relevant constraints unverifiable and misses the issue's useful case |

## Proposed execution model

1. A workflow surfaces candidate guardrails and metis for the current phase,
   outcome, files, or subsystem. This is recommendation, not activation.
2. A human or an already-authorized deterministic workflow explicitly selects
   guardrails. Metis cannot be selected as an executable rule; it must first be
   promoted through human-led curation.
3. RNA creates an ephemeral activation plan containing artifact IDs, immutable
   source revisions, selection provenance, scope, and direct validation refs.
   The canonical `.oh/guardrails/*.md` files remain the source of truth.
4. The runner reuses ADR validation's exact test/audit/smoke/script executors and
   structured result reporting, but applies no ADR status gate.
5. Missing checks are reported as `advisory/unverified`, never fabricated. A
   selected guardrail is blocking only when its canonical severity and the
   invoking workflow explicitly say so.
6. The plan and results are visible in the session or PR so reviewers can audit
   what was selected, why, and what ran.

The first implementation should avoid automatic semantic matching, persistent
per-user activation state, metis execution, and universal CI integration. Those
are separate policy decisions with materially larger blast radius.

## Concrete downstream workflow

During `/ship`, the workflow can explicitly activate executable guardrails that
govern the changed subsystem. A dependency-changing PR could activate
`ci-artifacts-for-release-builds`, the CodeRabbit approval requirement, and any
approved dependency-exception guardrail. The activation plan would run their
real checks and post the selected set plus results on the PR. This improves
behavior because a relevant rule becomes observable and reproducible rather
than relying on prompt recall, while unrelated guardrails remain inactive.

## Harmful over-enforcement case

A metis entry learned while salvaging a Rust concurrency change might recommend
serial execution or a particular test strategy. If RNA inferred that this metis
applies globally, a documentation-only or Python task could be blocked by an
irrelevant Cargo check. Worse, an old workaround could keep enforcing itself
after the underlying limitation disappears. This is why semantic retrieval may
surface candidates but cannot itself authorize execution.

## Decision and follow-up threshold

Proceed only with a future, separately reviewed implementation if it can prove
all of the following in a thin vertical slice:

- explicit activation is distinguishable from recommendation;
- only guardrails, not metis, can be executable;
- every check is a direct reference supported by the existing ADR runner;
- selection provenance and results are review-visible;
- no selected artifact means no new command runs; and
- a real workflow demonstrates both a relevant pass/fail and an unrelated rule
  remaining inactive.

Until those conditions are accepted, the current advisory and human-curation
workflows should remain unchanged.
