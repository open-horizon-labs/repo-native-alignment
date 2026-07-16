---
name: review
description: Check work and detect drift before committing. A second opinion that catches misalignment early. Use at natural pause points, before PRs, or when something feels off.
---

# /review

A second opinion before committing. Review checks alignment between intent and execution, detects drift, and decides: continue, adjust, or salvage.

This is part of the **Intent > Execution > Review** loop that runs at every scale--from a single commit to a multi-week project. Review closes the loop.

The same skill also provides an **independent review mode** for `/ship`. A fresh, separate sub-agent returns an explicit `APPROVE` or `REQUEST CHANGES`; when invoked for the final-diff gate, it reviews the exact commit proposed for merge.

## When to Use

Invoke `/review` when:

- **Before committing** - natural checkpoint before code hits the branch
- **At natural pause points** - end of a work session, end of a subtask
- **Something feels off** - you've been coding a while and lost track of where you are
- **Scope seems to have grown** - the task feels bigger than when you started
- **Before PRs** - final check before requesting review from humans

**Do not use when:** You're in deep flow and making progress. Don't interrupt productive work. Review at natural breaks, not arbitrary intervals.

## Independent Review Mode

Use this mode only when invoked by `/ship` as a fresh, separate reviewer sub-agent. Step 2 uses it for early adversarial feedback; step 10c invokes a different fresh reviewer for the binding final-diff gate.

### Isolation contract

- The reviewer must be a distinct sub-agent, not the implementer and not a reused earlier reviewer.
- Start with fresh context.
- Inputs are limited to:
  - the exact final diff and reviewed commit SHA;
  - the linked issue acceptance criteria;
  - relevant guardrails and metis;
  - RNA graph impact for the changed areas.
- Do not read or request the implementation session, implementation reasoning, prior conversation, or a summary defending the chosen solution.

### Review contract

1. Verify every acceptance criterion against the diff.
2. Check relevant hard and soft guardrails.
3. Check changed-area impact and likely regressions using the provided RNA context.
4. Identify concrete correctness, security, maintainability, testing, delivery, and policy-consistency findings.
5. Return exactly one binding verdict:
   - `APPROVE` — no unresolved actionable findings.
   - `REQUEST CHANGES` — one or more actionable findings must be fixed.

`COMMENT`, conditional approval, and approval with unresolved findings do not satisfy the merge gate.

### Required output

```text
## Ship Step [2 or 10c]: [Independent Code Review or Independent Final-Diff Review]

**Reviewer:** [fresh sub-agent task identifier]
**Reviewed commit:** [exact SHA]
**Verdict:** [APPROVE/REQUEST CHANGES]

### Acceptance Criteria
- [x/blank] ...

### Guardrails and RNA Impact
[what was checked]

### Findings
[actionable findings, or "No unresolved actionable findings."]
```

`REQUEST CHANGES` must be fixed. For the step 10c final-diff gate, fixes must be reviewed by another fresh sub-agent, and any diff change after `APPROVE` invalidates the approval and requires a new fresh reviewer on the new head commit.

## The Review Process

### Step 1: State the Original Aim

Before reviewing anything, restate what we set out to do:

> "The aim was: [original intent in one sentence]"

If you can't state the aim clearly, that's the first finding. Go find it or clarify with the user before proceeding.

### Step 2: Check Alignment (Five Questions)

Work through these checks against the original aim:

#### 1. Still Necessary?

Is this solving a real, current problem--not a hypothetical future one?

- Are we building something that's actually needed right now?
- Or did scope expand to "future flexibility" or premature optimization?

> If unnecessary work crept in: "This adds [X] which wasn't part of the original aim."

#### 2. Still Aligned?

Is the work still pointed at the original aim?

- Does what we're building actually address the stated problem?
- Are we on the critical path, or did we drift to tangents?

Signs of drift:

- "While I'm at it..."
- Refactoring unrelated code
- Solving problems the user didn't mention

> If misaligned: "The aim was X, but current work addresses Y."

#### 3. Still Sufficient?

Is the approach appropriately sized for the problem?

- Could this be done with less code, fewer files, less abstraction?
- Are we building infrastructure for a one-off task?

Complexity signals:

- RED: 3+ files for simple feature; new patterns for one-offs
- YELLOW: proliferating Manager/Handler/Service classes
- GREEN: direct solution; one file when possible; reuses existing patterns

> If over-complex: "A simpler approach would work. Consider [alternative]."

#### 4. Mechanism Clear?

Can you articulate WHY this approach works?

- Is there a clear "because" statement?
- If the mechanism can't be stated simply, the problem may not be understood

> If unclear: "What's the mechanism? Why will this solve the problem?"

#### 5. Changes Complete?

Are all ripple effects handled?

- New fields initialized everywhere the struct is created?
- Persistence changes have migration paths?
- Contract changes updated in all callers?

> If incomplete: "This adds [X] but doesn't update [related site]."

### Step 3: Detect Drift

Drift is the gap between where you started and where you are. Explicitly name any drift found:

**Scope Drift** - The task grew beyond original boundaries
**Solution Drift** - The approach changed from what was planned
**Goal Drift** - The aim itself shifted without explicit decision

For each drift detected:

```text
Drift: [type]
Started as: [original]
Became: [current]
Impact: [what this means]
```

### Step 4: Decide Next Action

Based on the review, choose one:

| Decision | When | Action |
| ---------- | ------ | -------- |
| **Continue** | Aligned, on track | Proceed with confidence |
| **Adjust** | Minor drift, recoverable | Correct course and continue |
| **Pause** | Unclear aim or major questions | Stop, clarify with user, then resume |
| **Salvage** | Significant drift, restart needed | Extract learning with `/salvage`, then restart |

## Output Format

```text
## Review Summary

**Aim:** [original intent]
**Status:** [Continue / Adjust / Pause / Salvage]

### Alignment Check
- Necessary: [Yes/No - brief note]
- Aligned: [Yes/No - brief note]
- Sufficient: [Yes/No - brief note]
- Mechanism clear: [Yes/No - brief note]
- Changes complete: [Yes/No - brief note]

### Drift Detected
[List any drift found, or "None detected"]

### Decision
[Reasoning for the status decision]

### Next Steps
[Concrete actions to take]
```

## Examples

### Example 1: Clean Review (Continue)

```text
/review

I've been working on the auth endpoint for 30 minutes.
```

**Output:**

```text
## Review Summary

**Aim:** Add JWT validation to the /api/users endpoint
**Status:** Continue

### Alignment Check
- Necessary: Yes - endpoint currently accepts any request
- Aligned: Yes - implementing exactly what was specified
- Sufficient: Yes - using existing auth middleware, minimal new code
- Mechanism clear: Yes - middleware validates token, rejects invalid
- Changes complete: Yes - endpoint updated, tests added

### Drift Detected
None detected

### Decision
Work is aligned and on track. JWT validation implemented using existing patterns.

### Next Steps
1. Run tests
2. Commit
3. Open PR
```

### Example 2: Drift Detected (Adjust)

```text
/review

Started adding error handling to the API. Now I'm refactoring the logger.
```

**Output:**

```text
## Review Summary

**Aim:** Improve error handling in /api/orders endpoint
**Status:** Adjust

### Alignment Check
- Necessary: Partially - error handling yes, logger refactor no
- Aligned: Drifted - logger refactor wasn't part of the aim
- Sufficient: Over-scoped - touching infrastructure for a single endpoint
- Mechanism clear: Yes for error handling, unclear for logger
- Changes complete: No - error handling incomplete

### Drift Detected
Drift: Scope Drift
Started as: Add try/catch and proper error responses to /api/orders
Became: Refactoring the entire logging system
Impact: Original task incomplete, unrelated system being modified

### Decision
Revert logger changes. Complete the original error handling task. If logger needs work, that's a separate task.

### Next Steps
1. `git stash` the logger changes
2. Complete error handling for /api/orders
3. Commit that work
4. If logger refactor is still needed, create separate task
```

### Example 3: Major Drift (Salvage)

```text
/review

Third attempt at fixing the build. Each fix breaks something else.
```

**Output:**

```text
## Review Summary

**Aim:** Fix failing CI build (test timeout issue)
**Status:** Salvage

### Alignment Check
- Necessary: Yes - build must pass
- Aligned: No - now touching unrelated config
- Sufficient: No - complexity increasing with each fix
- Mechanism clear: No - unclear why tests timeout
- Changes complete: No - in unstable state

### Drift Detected
Drift: Solution Drift
Started as: Increase test timeout
Became: Modifying test infrastructure, CI config, and mock setup
Impact: Build in worse state than when started, root cause unknown

### Decision
Stop. The approach has reversed multiple times. Extract what was learned about the test infrastructure and restart with better understanding.

### Next Steps
1. Run `/salvage` to capture learnings
2. Revert to known-good state
3. Investigate root cause before attempting fix
4. Start fresh with clear hypothesis
```

## Session Persistence

This skill can persist context to `.oh/<session>.md` for use by subsequent skills.

**If session name provided** (`/review auth-refactor`):

- Reads/writes `.oh/auth-refactor.md` directly

**If no session name provided** (`/review`):

- After producing the review summary, offer to save it:
  > "Save to session? [suggested-name] [custom] [skip]"
- Suggest a name based on git branch or the work being reviewed

**Reading:** Check for existing session file. Read **Aim** (what outcome we wanted), **Problem Statement**, **Solution Space** (approach taken), and **Execute** status. This is essential for detecting drift.

**Writing:** After review, write the assessment:

```markdown
## Review
**Updated:** <timestamp>
**Verdict:** [ALIGNED | DRIFTED | BLOCKED]

[review findings, drift analysis, recommendations]
```

## Adaptive Enhancement

### Base Skill (prompt only)

Works anywhere. Produces review summary based on conversation context. No persistence.

### With .oh/ session file

- Reads `.oh/<session>.md` for aim, constraints, selected solution
- Compares actual work against session aim
- Writes review verdict and findings to the session file

### With git diff

- Compares actual changes against stated aim
- Detects file count, complexity signals
- Identifies incomplete changes

### With CI Integration

- Checks if tests pass before marking complete
- Verifies linting, type checking
- Confirms PR checks would pass

### With Open Horizons MCP

- Pulls related past decisions for context
- Logs review outcomes to graph
- Session file serves as local cache

### With RNA MCP (repo-native-alignment)

- Call `search("constraints for [area]", include_artifacts: true)` to check relevant guardrails and metis
- Call `outcome_progress` to check work against the declared outcome
- Write approved new constraints to `.oh/guardrails/` with YAML frontmatter

## Completion Gate (Before "Done")

When the user or agent claims work is complete, verify:

1. **PR Intent Clear?** - Can you state what the PR delivers in one sentence?
2. **Changes Reviewed?** - Has the branch diff been reviewed against intent?
3. **CI Passing?** - Have automated checks been run and passed?
4. **Feedback Addressed?** - Have reviewer comments been resolved?
5. **Final Diff Approved?** - Did a fresh, separate repo-local `/review` sub-agent explicitly approve the exact final commit? Any later diff change invalidates approval and blocks completion until another fresh review.

If incomplete:
> "Completion gate: [missing step]. Run the check before marking complete."

## Position in Framework

**Comes after:** `/execute` (natural checkpoint after building).
**Leads to:** `/ship` if aligned, `/salvage` if drifted, back to `/aim` if fundamentals are unclear.
**This is the gate:** Review decides whether to continue, adjust, or restart.

## Leads To

After review, typically:

- **Continue** - Proceed to commit, PR, or next task
- **Adjust** - Make corrections, then continue
- **Salvage** - Run `/salvage` to extract learning before restart
- **Clarify** - Return to `/aim` or `/problem-statement` if fundamentals unclear

---

**Remember:** Review is not bureaucracy. It's the moment you catch drift before it compounds. Five minutes of review saves hours of wrong-direction work.
