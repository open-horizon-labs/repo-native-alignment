---
id: operation-report-control-plane
outcome: context-assembly
title: 'Operation Reports Belong at the Control-Plane Seam'
---

## Pattern

When scan/enrich readiness has to be visible in both CLI and MCP, the source of truth must be a structured control-plane report, not CLI strings.

The useful seam is an operation-level report that records phases, capability states, degraded query classes, diagnostics, and next-step commands. Renderers can then produce CLI text, markdown/MCP text, or JSON from the same data.

## The trigger that produced this

Option D (#669-#671) started as a request for scan summaries and `--timings`, but the surrounding design already had ADR-003's durable enrichment job ledger. The risk was creating a second, CLI-only readiness model that would drift from MCP `list_roots`/search readiness.

The implementation kept ADR-003 as the capability job ledger and added `OperationReport` as bounded diagnostic/control-plane history under `.oh/.cache/operation_reports.json`.

## Why this matters

CLI-only timing output answers a human's immediate question but fails RNA's own core requirement: if an agent cannot discover the same readiness/degradation state through MCP surfaces, the metadata is computed but not delivered.

The report model also avoids lossy `ready: bool` shortcuts. `completed`, `skipped`, `unavailable`, `failed`, `stale`, and `superseded` carry different operational meaning and different next actions.

## Application rule

Future status, doctor, verbose search, or root-readiness surfaces should consume `OperationReport`/ADR-003 job state instead of adding bespoke readiness strings.
