---
id: validate-before-building
outcome: agent-alignment
title: Validate the hypothesis before adding infrastructure — behavior change is the metric, not tool count
---

## What Happened

Session 1 (March 7): RNA had 16 tools, zero SLO observations. Added this as a guardrail to prevent premature infrastructure: "does an agent behave differently with the tools we already have?"

## Why It's Now Metis

The guardrail did its job. Since then:
- Consolidated 16 tools → 5 (tool-consolidation-shipped)
- Validated on 3 repos (three-repo-validation)
- Measured: ~50s + ~half tokens with RNA vs ~120s with LSP alone
- Measured: 1-7 RNA calls without directive vs 27+ with (rna-directive metis)

The premise ("16 tools, zero observations") is stale. The learning stands — validate before building — but it's no longer an active constraint on new features. Demoted from guardrail to metis.

## The Learning

When building infrastructure for agents, measure behavior change early. The temptation is always "one more tool." The discipline is: does the agent do something different? This saved RNA from bloating during the early build phase.
