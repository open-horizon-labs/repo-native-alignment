---
id: adversarial-testing-with-salvage
outcome: agent-alignment
title: 'Adversarial Testing: Dissent-Seeded, Salvage-Nudged'
---

## Pattern

After implementation and review/dissent, spawn adversarial test agents seeded with the dissent findings. The dissent tells them *where* the implementation was already challenged — they probe deeper rather than re-discovering known issues.

## Why It Works

1. **Dissent findings are test specifications.** Each review finding ("duplicate edges when Trait has both find_implementations and type hierarchy subtypes") is a concrete scenario the implementation must handle. Adversarial tests turn these into executable assertions.

2. **Functional > smoke > unit preference.** Testing the real code paths agents will hit (MCP tool handlers, ranking pipelines) catches integration bugs that unit tests miss. The case-sensitivity bug in PR #113 (`code:Function` vs `code:function`) would pass any unit test of the embedding logic — it only fails when the full pipeline connects.

3. **Salvage nudge forces design reflection.** Asking "what would we do differently in a do-over?" during test writing surfaces structural assumptions. Tests that expose design assumptions (e.g., "does score ordering remain stable when all scores tie?") are more valuable than tests that verify happy paths.

## The Pipeline

```
implement → review/dissent → fix → adversarial test → merit assessment → merge/abandon
```

Each stage feeds the next:

| Stage | Input | Output | Question Answered |
|-------|-------|--------|-------------------|
| implement | Solution space decision | PR with code | "Does it compile and pass tests?" |
| review/dissent | PR diff | Findings (bugs, design issues) | "Is the code correct?" |
| fix | Review findings | Updated PR | "Are the findings addressed?" |
| adversarial test | Dissent findings + salvage nudge | Edge case tests + bugs | "What breaks under pressure?" |
| merit assessment | PR diff + real usage scenarios | MERGE/ABANDON verdict | "Is this worth merging?" |
| resolve TODOs | All PR comments + caveats | Fixed or explicitly N/A with reasoning | "Is everything accounted for?" |
| manual verification | Built binary + real data | Before/after results posted to PR | "Does it actually work?" |
| merge/abandon | All steps complete | Decision | "Ship it or kill it?" |

The adversarial test agent receives:
- The PR diff (what was built)
- The review/dissent comments (what was challenged)
- A mandate to think like a dissenter + salvager

The merit assessment agent receives:
- The PR diff
- Real before/after scenarios grounded in the project's aim
- A mandate to answer "would agents actually benefit?" — not "is the code clean?"

**Key gap this pipeline fills:** Review/dissent answers "is the code correct?" but not "is the feature valuable?" Those are orthogonal — perfectly tested code that doesn't deliver outcome value should be abandoned, not merged.

## Learned From

Session where 6 parallel PRs (#112-#117) went through this pipeline. Key bugs found by adversarial testing that review/dissent missed:
- Token budget violations only visible when constructing realistic embedding text
- Cache invalidation gaps (HEAD same but working tree dirty)
- Score stability under ties (implementation-dependent ordering)
- Cross-source edge dedup correctness when edges have different directionality

## Anti-Pattern

Don't run adversarial tests without the dissent seed. Unguided adversarial testing produces shallow "what if null?" tests. Dissent-seeded testing produces "what if the type hierarchy query returns the node itself as its own supertype?" — tests grounded in actual implementation decisions.
