---
type: friction-log
issue: 728
status: active
date: 2026-07-14
---

# Issue #728 execution friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-14 | RNA worktree search | minor | Branch-local RNA reported the correct schema-version symbol but omitted the inline source comment, so the first exact patch context failed. | Required narrow `git show HEAD:<path>` reads to recover exact patch anchors; no code was changed by the failed patch. | Include the complete declaration text in compact symbol results or offer a source-span read primitive. |
| 2026-07-14 | RNA nested-worktree search | minor | The dirty content-native worktree had no persisted graph, and parent-index queries exposed only part of its provenance implementation. | The branch was treated as non-authoritative reference material; implementation stayed in the clean #728 worktree. | Prewarm each active worktree independently and avoid parent-index nested-worktree leakage. |
| 2026-07-14 | RNA shell-script search | minor | RNA did not index the top-level function bodies or executable sections in `scripts/test-suite.sh`. | Required narrow source-span reads to place the release fixture and assertions. | Index top-level shell functions and executable statements so release scripts are explorable through RNA. |
