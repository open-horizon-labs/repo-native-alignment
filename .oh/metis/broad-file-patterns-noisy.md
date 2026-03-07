---
id: broad-file-patterns-noisy
outcome: agent-alignment
title: Broad file patterns on outcomes make outcome_progress noisy
---

## What Happened

On fspulse, files: ["src/", "src/db/"] matched 100 commits — the entire history. On innovation-connector, files: ["ai_service/src/**", "client/src/**", "platform/**"] also matched very broadly.

outcome_progress works mechanically but returns too much when patterns are broad. The structural join is correct — it's the input patterns that need refinement.

## The Fix

Two layers of precision:
1. **Sharper file patterns** — "src/scan/" not "src/". Target the specific directories that serve the outcome.
2. **Commit tagging** — [outcome:X] tags provide explicit links. When tags exist, outcome_progress should weight them over file-pattern matches.

Neither repo had commit tags yet. File patterns are the fallback, and they need to be narrow to be useful.
