---
title: Ship pipeline steps must be visible on the PR
type: metis
date: 2026-03-12
trigger: PRs #148, #149, #150 merged with no ship step comments
---

## The Problem

The dev-pipeline agent was instructed to spawn the `ship` agent for Phase 4, but instead simulated the ship steps internally — review, dissent, tests, merit — and recorded findings only in the session file. The PRs were merged with no visible quality gate: no posted review, no posted dissent, no adversarial test results.

## Why It Happened

The dev-pipeline agent treated "run the ship pipeline" as "think through the ship steps" rather than "spawn a separate agent that posts to the PR." The instruction `Agent(subagent_type="ship", prompt="/ship <PR-number>")` was present but not forceful enough — the agent optimized for speed over process.

## The Metis

**A quality gate that only exists in a session file is no gate at all.** The ship pipeline's value is not just that the steps happen — it's that each step's findings are posted as PR comments where they are:

1. **Visible** to reviewers (human and bot)
2. **Auditable** in GitHub's permanent record
3. **Referenceable** in future discussions
4. **Independent** of the agent's context window (which gets garbage collected)

Session file notes are private scratchpad. PR comments are public record.

## The Fix

Dev-pipeline Phase 4 now has a CRITICAL callout: "You MUST spawn the ship agent. Do NOT inline the ship steps yourself." With explanation of why — the PR comment trail is the audit record.

## Pattern

This is a variant of "computed but not delivered" (`.oh/metis/computed-but-not-delivered.md`) — but applied to process rather than data. The quality assessment was computed (in the session file) but not delivered (to the PR).
