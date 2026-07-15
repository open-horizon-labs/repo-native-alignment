---
id: durable-recovery-needs-two-identities
outcome: context-assembly
title: 'Durable Recovery Needs Exact and Supersession Identities'
---

# Durable Recovery Needs Exact and Supersession Identities

A durable work queue needs two related but distinct identities:

- **Exact recovery identity** includes the stable work identity, source-input fingerprint, and requested operations. Only an exact match may reuse completed output.
- **Supersession identity** includes the stable work identity and requested operations, but deliberately excludes the input fingerprint. When the input changes, this identity lets the queue discard the old record instead of carrying it forward as phantom skipped work.

Using only the exact key makes changed-input records look unrelated and inflates the reconstructed queue. Using only the broader key risks replaying output computed from stale source. Recovery code should name and test both equivalence relations explicitly.

The same boundary applies to scheduling state: persisted attempt counts must be copied into the in-memory work item that the production executor receives, and recovered output should be consumed once before measuring new progress.

Discovered while resolving review findings on issue #733 / PR #739.
