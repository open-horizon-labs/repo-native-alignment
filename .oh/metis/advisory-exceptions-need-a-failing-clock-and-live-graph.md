---
title: Advisory exceptions need a failing clock and live graph
date: 2026-07-15
outcome: context-assembly
source_issue: 736
---

A dependency-warning exception is not deterministic merely because it records
an advisory ID and expiry. If the same warning remains while the parent graph
moves, static path prose can become false without changing the audit finding.

For each supported feature scope, record the exact direct dependents and
recompute them during the live release audit. The exception then fails on three
independent kinds of drift:

1. cargo-audit finds a new advisory, package version, or warning kind;
2. the dependency graph gains or loses an owning parent in any feature scope;
3. the human decision expires.

This makes a time-bounded exception a failing clock plus an executable graph
claim. It does not turn an informational maintenance warning into a
vulnerability, and it does not pretend the warning was removed. It makes the
reason for carrying it reviewable until a compatible parent migration exists.
