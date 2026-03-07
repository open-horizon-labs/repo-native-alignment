---
id: three-repo-validation
outcome: agent-alignment
title: 'Three-repo validation: the system generalizes'
---

## Evidence

Tested on three repos with different shapes:
1. **repo-native-alignment** (Rust, self-referential, built for this)
2. **fspulse** (Rust CLI, personal project, cold start)
3. **innovation-connector** (enterprise app, Python+TS monorepo, multi-contributor, Azure DevOps work items)

All three: oh_init scaffolded .oh/, oh_search_context found relevant context, outcome_progress mapped commits to outcomes via file patterns.

## Key Findings

1. **Structural joins generalize.** outcome → files: patterns → git commits works regardless of language, framework, or project structure. Depends on someone writing reasonable files: globs.

2. **Broad file patterns make outcome_progress noisy.** fspulse matched 100 commits (entire history) because files: had "src/" and "src/db/". innovation-connector had similar breadth. Need sharper patterns or commit tagging to be precise.

3. **[outcome:X] tags not adopted yet.** Neither fspulse nor innovation-connector had tagged commits. The structural join falls back to file-pattern matching only. Commit tagging is the precision layer.

4. **Cold-start is minutes, not hours.** Even without grounded oh_init, manual scaffolding was fast. The P6 oh_init improvement would make it seconds.

5. **Signal gaps are the actionable finding.** fspulse had four optimization phases but zero signal measurements. innovation-connector had signals defined but no observations. The agent surfaced these gaps without being told to look.

## Dissent Status

The PR #1 dissent asked for second-repo testing and agent behavior validation. This provides both. The system surfaces real gaps on codebases it's never seen.
