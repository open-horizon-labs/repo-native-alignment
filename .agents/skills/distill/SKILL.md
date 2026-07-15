---
name: distill
description: Human-led curation of accumulated metis and guardrails. Surface patterns across sessions, propose what to promote, compact, or dismiss. Use after multiple sessions, before a new phase, or when search results feel noisy.
---

# /distill

Surface patterns in accumulated knowledge. Propose what to keep, promote, compact, or dismiss. The human decides.

Distill is the complement to `/salvage`: salvage extracts learning from a single session; distill curates the corpus those extractions build over time. Without distill, metis accumulates but never compounds.

## When to Use

Invoke `/distill` when:

- **After multiple sessions** - The corpus has grown and hasn't been reviewed
- **Before a new phase** - Want to know what's actually settled before moving forward
- **Search results feel noisy** - RNA `search` returns too much loosely-related content
- **Similar learnings keep appearing** - `/salvage` keeps extracting the same insights (the meta-signal)
- **End of a successful session** - Even good sessions produce learnings worth capturing before context is lost

**Use distill (not `/salvage`) when the session went well.** `/salvage` is for stopping because things went wrong. Distill is for pausing because things went right — or simply finished — and learnings are worth capturing before context is lost.

**Do not use when:** You're in the middle of execution. Distill is a pause point, not a mid-flight activity.

## The Human-Led Curation Principle

Distill leverages LLMs for what they're good at — pattern recognition, clustering, surfacing similar entries — while keeping judgment with the human.

**LLMs do:** find recurring themes, group similar entries, surface candidates.
**Humans do:** decide what matters, what's worth promoting, what's stale or context-specific.

Auto-promotion is never correct. A theme proposal is not a guardrail until a human writes it.

## The Process

### Step 1: Establish Scope

Decide what corpus to work with:

- **Session scope** (default, no RNA needed): learnings from this conversation
- **Corpus scope** (RNA available): accumulated metis across all sessions, optionally filtered by outcome, phase, or tag

### Step 2: Surface Candidates

**Session scope:** Review the conversation. What was learned? What assumptions were validated or invalidated? What constraints were discovered? What would be useful to know at the start of the next session?

**Corpus scope (RNA):** Call `search` broadly with `include_artifacts: true`. Cluster returned artifacts by semantic similarity. Identify:

- Entries that appear together repeatedly (candidates for compaction)
- Patterns across entries (candidates for guardrail promotion)
- Entries that contradict each other (candidates for resolution)
- Entries that are stale or overly context-specific (candidates for dismissal)

### Step 3: Present for Human Review

Present each candidate group with four possible actions:

```text
**Theme: [theme name]**
Entries: [list with source IDs and one-line summaries]
Suggested action: [Keep / Promote / Compact / Dismiss]
Reason: [why this action fits]
→ Your call:
```

**Keep** — leave as individual metis entries, no change.
**Promote** — pattern is recurring and stable enough to warrant a guardrail.
Distill drafts a stub; the human reviews, edits as needed, and explicitly
approves the final content. Only then may an agent write the file:

  ```markdown
  ---
  id: [slug]
  outcome: [outcome-id]
  severity: soft
  title: [one-line constraint]
  ---
  [drafted body — human refines, agent writes to .oh/guardrails/]
  ```

**Compact** — multiple entries say the same thing. Human approves a merged version; originals archived or deleted.
**Dismiss** — stale, superseded, or so context-specific it misleads more than it helps.

### Step 4: Write Results

Only write what the human approved. No auto-promotion, no auto-deletion.

- Every created or modified `.oh/**/*.md` artifact must have valid YAML
  frontmatter in deterministic BTreeMap key order.
- New metis entries → `.oh/metis/<slug>.md` after human approval.
- Guardrail candidates → human reviews/edits and explicitly approves the draft;
  only then may the agent write `.oh/guardrails/<slug>.md`.
- Compactions → write an approved merged entry with frontmatter and note which
  originals the human may remove.
- Session compaction → preserve valid frontmatter; offer to remove stale planning
  artifacts while keeping settled decisions as brief anchors.

## Output Format

```markdown
## Distill Summary

**Scope:** [session | corpus — filtered by: outcome/phase/tag]
**Entries reviewed:** [N]

### Proposals

**[Theme or entry title]**
- Source(s): [file paths or conversation reference]
- Suggested: [Keep / Promote to guardrail / Compact / Dismiss]
- Reason: [one sentence]
→ Decision: [human fills this in]

[repeat for each proposal]

### Results Written
- [what was actually written, with file paths]
```

## Guardrails

- **Never auto-promote.** A theme proposal is not a guardrail until a human writes it.
- **Never auto-delete.** Dismissal proposals require human confirmation.
- **Preserve provenance.** Every proposal links to source metis IDs or conversation context.
- **Phase-aware.** Corpus-mode clustering must surface phase tags — cross-phase metis often misleads. A solution-space learning is not automatically relevant in problem-space.
- **Graceful with sparse corpus.** With fewer than 5 entries, surface what exists without manufacturing false patterns. Don't cluster noise.
- **Metis is contextual, not universal.** What worked in one context doesn't carry everywhere. The human selects what applies; distill surfaces candidates.

## Adaptive Enhancement

### Base Skill (prompt only)

Reads the current conversation. Surfaces what was learned this session. Proposes metis entries for human review and approval. Works after any completed session — the positive complement to `/salvage` (which is for drift and failure).

Output: approved entries as markdown, ready to write to `.oh/metis/`.

### With .oh/ session file

- Reads `.oh/<session>.md` for session context and prior metis
- Writes approved metis entries directly to `.oh/metis/`
- After distill, offers to compact the session file itself: remove stale planning content, keep settled decisions as brief anchors
- The compacted session file seeds future sessions more cleanly

### With RNA MCP (repo-native-alignment)

Corpus mode: RNA `search` across all accumulated metis (or a filtered subset), cluster by semantic similarity, and surface candidate groups for human review.

Output is PR-able: distill produces a set of proposed file writes (new guardrail stubs, compacted metis, removal notes) formatted as a markdown summary. The human creates a branch, writes the approved files, and opens a PR — or uses `/oh-plan` to create issues for each promotion. Curation becomes a collaborative act, not a solo one.

**Filtering options:** include an outcome ID, phase tag, or recency window in the `search` query to narrow the corpus. Useful when only a domain slice needs curation.

## Position in Framework

**Comes after:** Multiple sessions of `/salvage` or `/execute` that have accumulated metis. Or: end of any session where learnings are worth capturing before context is lost.
**Leads to:** Cleaner RNA search results. Guardrail candidates for human authoring. A compacted session file that seeds the next session.
**Relationship to `/salvage`:** Salvage is per-session extraction (especially from failure). Distill is corpus-level curation (independent of any single session outcome). They're complementary — salvage feeds the corpus; distill keeps it from becoming noise.

## Leads To

After distill, typically:

- Write approved guardrail files to `.oh/guardrails/`
- Open a PR with `.oh/` changes for team review (corpus mode)
- Return to `/aim` or `/problem-space` with a cleaner, more settled context

---

**Remember:** Distill is not synthesis. The LLM finds patterns; you decide what matters. A corpus that accumulates without curation becomes noise. A corpus that's periodically distilled becomes situated judgment — the kind that actually improves future sessions.
