---
id: investigate-before-fixing-stale-edges-non-issue
outcome: agent-alignment
title: 'Investigate first: stale-edge issue (#36) was non-issue — cleanup already correct by design'
---

## What Happened

Issue #36 was filed as "stale Calls edges not cleaned up when source file changes." PR #40 investigated and found the issue doesn't exist: the `from.file` cleanup path already removes all edges originating from a file when that file is re-indexed. The existing implementation was correct.

## Why It Matters

The investigation was worth doing — it produced a documented non-finding (PR #40). But the pattern is: opening a bug issue and building a fix without first confirming the bug exists wasted potential implementation time.

## The Check

Before implementing a fix for an alleged data correctness issue:
1. Find the existing deletion/cleanup path in the code
2. Trace what it deletes (what invariants it maintains)
3. Verify whether the alleged bug can actually occur given those invariants

In this case: `from.file` cleanup removes all graph edges where the source node is in the changed file. Calls edges have source nodes in the changed file. Therefore Calls edges are cleaned up correctly. The investigation would have been 15 minutes; the PR to "fix" it would have been hours.

## Evidence Source

PR #40 (investigated as non-issue), issue #36 investigation.
