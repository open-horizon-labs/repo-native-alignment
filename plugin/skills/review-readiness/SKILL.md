---
name: review-readiness
description: Agent-led PR/working-tree review triage. Start from the diff, add RNA graph/business context only where it changes review decisions, and state when raw git diff is enough.
---

# /review-readiness

Decide whether RNA context makes a PR or working-tree diff easier to review, then produce the smallest useful review map.

This is a **skill**, not a fixed report generator. The agent chooses the probes that fit the change. The point is not to prettify `git diff`; it is to answer review questions that raw diff cannot answer cheaply:

- Which changed symbols are structurally important?
- Which callers, outcomes, guardrails, or docs should a reviewer inspect?
- Which missing metadata actually blocks review, and which does not matter?
- Where is RNA adding value, and where is plain diff already the right artifact?

If RNA adds no value for the current diff, say that directly and review from the diff.

## Arguments

`$ARGUMENTS` may identify the change to inspect:

- empty: current working tree diff
- `--base main`: `main...HEAD`
- `--base <sha1> --head <sha2>`: explicit range
- `--pr 660`: GitHub PR diff

## Procedure

### Step 1: Get the diff shape

Use the cheapest source that matches the argument:

- working tree: `git diff --stat` and targeted `git diff -- <path>` as needed
- branch vs base: `git diff --stat <base>...HEAD` and targeted hunks
- PR: `gh pr view` for title/body/base/head, then `gh pr diff` or local diff by resolved refs

Do not paste the whole diff unless the user asks. Summarize:

- changed files by kind: code, tests, docs, `.oh/` artifacts, config
- added/modified/deleted/renamed shape
- whether the diff is mostly behavioral code, review process, docs, tests, or generated/artifact content
- changed hunks with current-side line ranges when available (`+start,count`) and deleted-only hunks called out explicitly
- for code hunks, whether each hunk is mapped to current extracted symbols, unmapped, deleted-only, docs/config-only, or not covered by the indexed graph

### Step 2: Decide whether graph context is worth using

Ask this before querying RNA: **what review decision would graph context improve?**

Use RNA when at least one is true:

- a changed function/type/module has non-obvious callers or dependents
- a public/exported surface changed
- the diff touches extractor, graph, scanner, service, server, or query boundaries
- the diff appears related to an outcome, guardrail, ADR, or previous metis
- the reviewer needs impact/risk more than line-by-line commentary

Skip RNA and say so when:

- the diff is only docs, session notes, or skill prose
- the only executable change is self-contained and obvious from the diff
- the graph has no indexed symbols for the changed language/file and that absence would not affect the review decision

### Step 3: Probe selectively

Use repo-native tools before generic grep/read for code understanding.

Useful probes include:

- `repo_map` for unfamiliar subsystem orientation
- `search(query=<symbol/file/outcome>, compact=true)` to find changed symbols or related artifacts
- `search(node=<id>, mode="neighbors", direction="both")` for local graph context
- `search(node=<id>, mode="impact")` when exact downstream impact changes review risk
- `outcome_progress(outcome_id=<id>)` when the diff claims outcome progress
- For code files, map current-side hunk ranges to current extracted graph symbols where the index has line ranges. Record the stable node ID for each mapped symbol. If a hunk is deleted-only, top-of-file/import-only, docs/config-only, or outside indexed coverage, keep it at file/hunk level with an explicit reason instead of forcing a symbol mapping.

For a small diff, two or three targeted probes are usually enough. Do not run full-repo LSP, embeddings, or broad enrichment as part of this skill.

### Step 4: Classify review readiness

Classify what the reviewer can trust:

- **Ready** — diff and context are enough to review now
- **Partial** — review can proceed, but named follow-up probes would reduce risk
- **Blocked** — a specific missing fact prevents an honest review

Be precise about missing metadata. Avoid generic complaints like “LSP not ready.” Instead say what is missing: exact callers for `X`, base-side identity for a deleted symbol, graph coverage for Python skill files, stale scan for `src/extract`, etc.
When RNA verbose output includes `review-readiness scoped context`, use that workflow-specific line instead of treating global `dead-code prerequisites` as the review gate. Scoped/root/changed-file LSP coverage is partial for global analysis but can still be enough for review when the missing facts are named.

### Step 5: Produce a review map

Output this shape:

```markdown
## Review Readiness

### Diff Shape
- [files/change categories]


### Hunk Map
| File | Hunk range | Classification | Symbol/node ID or reason |
|------|------------|----------------|--------------------------|
| [path] | [+start,count / deleted-only / n/a] | [mapped / unmapped / deleted-only / docs / config] | [stable node ID or explicit gap] |
### RNA Added Value
- [specific graph/business context that changes review decisions]
- or: RNA adds no material value for this diff; raw diff review is sufficient because [reason].

### Review Anchors
- [symbol/file/outcome/guardrail]: [why reviewer should inspect it]
- [hunk range → stable node ID or explicit unmapped reason]: [why reviewer should inspect it]

### Readiness
- Ready: [what can be reviewed now]
- Partial/Missing: [specific missing facts, if any]
- Not needed: [metadata intentionally skipped]

### Recommended Next Step
- [continue review / run targeted probe / ask for scan refresh / treat as docs-only]
```

## How this ties to #659

#659 says enrichment readiness cannot be one global state. This skill makes readiness job-specific for PR review: start from the user's review task, pull only metadata that helps that task, and keep expensive providers such as LSP scoped to questions they uniquely answer.

The walking skeleton is the **agent process**. If repeated use exposes a hard mechanical step that agents cannot perform reliably, productize that one primitive in core RNA later.

## Guardrails

- Do not claim semantic completeness from line overlap or a clean diff.
- Do not run global dead-code analysis from this skill.
- Do not require full-repo LSP before first-pass review.
- Do not hide when RNA adds no value; that is a valid readiness result.
- Do not replace tests, compiler checks, or the mandatory independent final-diff `/review` approval.
