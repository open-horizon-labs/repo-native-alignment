---
name: review-readiness
description: Map a PR or working-tree diff to RNA graph context using cheap metadata first. Reports what review context is ready, partial, or missing without requiring full-repo LSP or embeddings.
---

# /review-readiness

Produce a review-readiness map for a diff. This is a job-shaped capability probe, not a prettier `git diff`.

The job: help an agent reviewing a PR or working-tree diff answer:

- What changed?
- Which extracted graph symbols does the change touch?
- What graph context is already available?
- Which metadata is missing, stale, or not needed?
- Is targeted enrichment worth running next?

This skill intentionally starts with cheap sources:

1. git diff / PR diff
2. RNA extracted graph symbol ranges
3. already-rendered RNA graph context (stable IDs, parent scopes, edge counts)

It does **not** run full-repo LSP, embeddings, or before/after graph snapshots.

## Arguments

`$ARGUMENTS` may be:

- empty: inspect the current working tree diff
- `--base main`: inspect `main...HEAD`
- `--base <sha1> --head <sha2>`: inspect an explicit git range
- `--pr 660`: inspect a GitHub PR diff

Examples:

```bash
/review-readiness
/review-readiness --base main
/review-readiness --pr 660
```

## Procedure

### Step 0: Keep the scope honest

This skill reports first-pass review readiness. It must not claim semantic completeness.

Do not:

- run full-repo LSP enrichment
- run embeddings
- infer deleted symbols without base-side symbol data
- treat line-overlap as exact semantic binding
- emit global dead-code findings

### Step 1: Generate the readiness report

Run the helper from the repo root:

```bash
python3 plugin/skills/review-readiness/review_readiness.py $ARGUMENTS
```

If the helper fails because `repo-native-alignment` is unavailable, tell the user RNA must be installed and the repo must have an extracted graph. Do not silently fall back to plain `git diff` as if it were equivalent.

### Step 2: Interpret the report

The report has four sections:

1. **Summary** — changed files/hunks and how many hunks mapped to extracted symbols.
2. **Changed Symbols** — graph-join anchors: symbol name, kind, stable node ID, parent, edge counts, signature.
3. **File / Hunk-Level Changes** — honest representation for unmapped, deleted-only, docs, config, or non-symbol hunks.
4. **Readiness** — what is ready, partial, not run, or unavailable.

### Step 3: Decide if review can proceed

Proceed with first-pass review when:

- changed files/hunks are available from git diff
- most code hunks map to extracted symbols or are honestly represented as file-level changes
- graph context is available for the important mapped symbols
- missing semantic refs/callers are not required for the immediate review decision

Pause and recommend targeted follow-up when:

- a changed exported/public symbol needs exact incoming callers
- deleted symbols are central and base-side identity is unavailable
- many hunks are unmapped in code files
- current graph appears stale relative to the diff

### Step 4: Report in review language

Summarize for the user:

```markdown
## Review Readiness

### Ready
- Changed files/hunks from git diff
- Current symbol overlap for mapped code hunks
- Existing RNA graph context for mapped symbols

### Partial / Missing
- Exact incoming semantic refs/callers not refreshed
- Deleted symbol identity unavailable without base graph
- Unmapped hunks need file-level review

### Recommended Next Step
- Continue review / run targeted impact for <symbol> / refresh extract-only graph / inspect unmapped hunks
```

## How this ties to #659

This skill is the first walking skeleton for capability-scoped enrichment readiness:

- The workflow is PR/diff review.
- The cheap capabilities are git diff + extracted graph.
- LSP and embeddings are explicitly not prerequisites.
- Readiness is reported for the review job, not as one global enrichment state.

If this skill proves useful but awkward, productize the hard parts in core RNA later: efficient symbol-range lookup, a first-class ChangeSet structure, or MCP rendering. Do not start there.

## Limitations

- Line overlap is detection, not proof of semantic binding.
- Deleted-only hunks need base-side symbol extraction to identify removed symbols.
- Large enclosing functions can swallow many small hunks.
- Current graph ranges may be stale if the repo changed since the last scan.
- Exact cross-file callers/references still require targeted semantic lookup when the review job needs them.
