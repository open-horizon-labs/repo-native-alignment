---
id: degraded-enrichment-needs-a-durable-negative-proof
outcome: context-assembly
title: 'Degraded enrichment needs a durable negative proof'
---

Preserving partial enrichment output is only half of an honest abort contract. The system must also durably preserve why that output is incomplete.

An in-memory degraded flag disappears at restart. If the persisted graph contains some LSP edges and the job ledger says only `completed`, readiness inference can incorrectly promote partial coverage to ready. Likewise, writing a full-enrichment sentinel suppresses the retry that could repair coverage.

For terminal-but-incomplete enrichment, persist an explicit degraded job state with the original diagnostic, finalize and store the partial graph, and withhold the completion sentinel. This lets queries use the partial graph while readiness fails closed and later runs remain eligible to repair it.
