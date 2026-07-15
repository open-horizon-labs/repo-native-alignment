---
id: durable-recovery-requires-output-and-input-identity
outcome: context-assembly
title: 'Durable recovery requires both output and input identity'
---

A terminal work-item state is not enough to resume enrichment safely. Skipping a completed item also requires its durable graph output; restoring that output requires proof that the node input and requested operations are unchanged. Legacy records without output or an input fingerprint must replay conservatively.

Retry exhaustion is part of capability truth, not just queue telemetry. Exhausted work must remain visible and make readiness fail closed, or a zero-invocation recovery can appear successfully complete while graph coverage is missing.

The useful regression seam is the production scheduler: seed a mixed persisted queue, restart it, count scheduled invocations, and assert completed work runs zero times while the interrupted item runs exactly once.
