---
id: 768-oh-task-friction
outcome: context-assembly
title: 'Issue 768 workflow friction'
---

# Issue 768 workflow friction

| Time | Tool path | Severity | Friction | Impact | Follow-up |
|---|---|---|---|---|---|
| 2026-07-16 | `but status -fv` | skipped | GitButler reported that setup requires a `gitbutler/*` branch, while this task was required to start from `origin/main` without a worktree. | The repository-explicit `git` commands from `/oh-task` were used for branch, commit, and push operations. | Decide whether ordinary RNA issue branches should be initialized for GitButler or exempted explicitly. |
| 2026-07-16 | `gh pr create --draft` | friction | GitHub rejected a draft PR with no commits between `main` and `issue/768`. | A process-only empty commit was required before the draft PR could exist; no implementation file changed first. | Encode this empty-commit fallback in `/oh-task`. |
| 2026-07-16 | RNA scan gate | friction | The required full scan finalized degraded because `typescript-language-server` did not reach quiescence. | Exact symbol, artifact, and source-span search remained usable, but repo-wide LSP coverage was partial. | Diagnose TypeScript initialization separately; it did not block the bounded Rust/Python probe. |
| 2026-07-16 | RNA exact source spans | friction | Two initial source-span requests exceeded the 200-line hard cap. | The requests were narrowed and then succeeded; no raw code-read fallback was used. | Surface the maximum in CLI help/error-adjacent examples. |
