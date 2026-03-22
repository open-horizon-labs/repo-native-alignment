---
id: codebase-to-warehouse-pipeline
status: proposed
oh_endeavor_id: cbd85d73-91f4-4bf3-90f8-88a69bfbe9d1
---

# Codebase-to-Data-Warehouse Pipeline

Connect code changes to business performance metrics. When a PR ships, automatically
extract structured signals (feature additions, bug fixes, performance changes, API
changes) and write them to a data warehouse alongside business metrics (DAU, revenue,
latency, error rates). Make "did this PR cause a crash in business metrics?" trivially
answerable.

## The gap

Business metrics (DAU, revenue, error rates) are well-served by existing analytics
tools. The missing piece is **code artifact extraction**: structured, queryable signals
about what changed in the codebase — which subsystem, what kind of change, which
functions, which API surface.

Today this extraction is not feasible. There is no tool that produces:
```
{
  pr_id: 483,
  merged_at: "2026-03-22T04:01",
  subsystems_touched: ["payments", "auth"],
  change_type: "fix",
  functions_changed: ["process_payment", "validate_session"],
  api_endpoints_changed: ["POST /payments"],
  complexity_delta: -12
}
```

...as a structured row that a data warehouse can JOIN against business metrics.
Engineers launch features and wait weeks to find out if they worked — not because
the analytics pipeline is slow, but because the **code side of the join doesn't exist**.

## The insight

RNA already extracts structured data from every PR: what changed, which functions,
which subsystems, what kind of change (fix, feat, refactor). The event bus (#479) would
make this streamable. An agent-to-warehouse pipeline would write RNA signals alongside
business metrics, enabling automatic correlation.

## What this enables

- "Which PR most correlates with the DAU drop on March 22?"
- "Show me all PRs that touched the payment subsystem in the week before the revenue dip"
- "What's the average time between a bug fix in auth and a reduction in support tickets?"

## Architecture sketch

```
RNA extraction events
  → ExtractionConsumer (warehouse writer)
  → time-series store (BigQuery, ClickHouse, DuckDB)
  ← business metrics (Amplitude, Mixpanel, Grafana)
  
JOIN: code_events ON date WITHIN metric_events
→ correlation analysis
→ agent-queryable: "what shipped before the crash?"
```

## Related

- RNA event bus (#479) — streaming extraction events
- Per-repo agent + centralized store (#454) — multi-tenant architecture
- diff extractor (deferred) — per-commit change signals
