# Session: repo-native-alignment

## Aim
**Updated:** 2026-03-06

**Aim:** Agents working in a codebase stay aligned to declared business outcomes — not just code correctness — because the outcomes, constraints, and progress signals live *in the repo* as queryable, evolving artifacts that agents read from and write to during normal work.

**Current State:** Agents operate in a vacuum. CLAUDE.md tells them code conventions. Maybe there's a README with project goals. But there's no structured, queryable record of: what business outcome are we optimizing for? What signals tell us we're on track? What have we tried? What did we learn? Agents rediscover intent every session from scattered prose, or worse, from the user re-explaining it.

**Desired State:** An agent starting a session can query: "What are the active business outcomes for this project? What SLOs are defined? What's the current state of each? What constraints apply? What have we learned?" — and get structured answers from the repo itself. When work is completed, the agent (or harness) records what was done towards which outcome, with what signal. Over time, the repo accumulates *situated judgment* — metis — that compounds.

### Mechanism

**Change:** A harness (tooling + conventions + repo structure) that:
1. Stores **outcomes** (aims), **signals** (SLOs/feedback), **constraints** (guardrails), and **learnings** (metis) as structured artifacts in the repo
2. Makes them **queryable** by agents at execution time (not just readable prose — structured enough to filter, match, and reason over)
3. **Evolves** through normal work — agents contribute observations, humans curate and promote

**Hypothesis:** Agent reliability improves when alignment context is explicit, queryable, and co-located with the code — because the agent doesn't have to infer intent, it reads it. The same structure that makes *code* semantically queryable (CodeState) applies to *business context*: outcomes, signals, and constraints are objects, not strings.

**Assumptions:**
1. Markdown + structured frontmatter in the repo is sufficient as the storage format (agents already parse markdown natively; git provides versioning and history)
2. The harness is lightweight enough that people actually use it (if it's heavier than writing a CLAUDE.md section, adoption fails)
3. Agents can meaningfully reason over outcome/signal/constraint triples at execution time (not just "read the file" — actually scope decisions by them)
4. The value compounds — metis from early sessions genuinely improves later sessions

### Feedback

**Signal:** An agent, given a task, can answer "which outcome does this serve?" and "what constraints apply?" without the user explaining it. Measurable by: does the agent scope its work correctly without re-prompting?

**Timeframe:** Immediate per-session. Compound effect visible after 5-10 sessions of accumulated metis.

### Guardrails

- **Repo-native**: no external store, no platform dependency. If you `rm -rf .oh/`, you lose context but nothing breaks.
- **Lightweight**: adding an outcome is writing a markdown file, not configuring a system.
- **Git-versioned**: every change to outcomes/signals/constraints is a commit. History is free.
- **Agent-writable but human-curated**: agents can propose metis; humans promote it to guardrails.
- **Not a replacement for OH graph**: this is the *local, repo-embedded* slice of what OH does at the organizational level. They can sync.

---

## Convergence with CodeState

CodeState and this aim are the same system viewed from two angles:

| CodeState asks | This aim asks |
|---|---|
| How do we make *code structure* queryable so agents stop making dumb mistakes? | How do we make *business intent* queryable so agents stay aligned to outcomes? |

The answer is the same architecture:
- **Structured artifacts** in the repo (code symbols <-> outcome/signal/constraint definitions)
- **Queryable via agent tooling** (MCP tools)
- **Git-aware** (symbol history <-> outcome evolution over commits)
- **Embeddings for semantic search** ("find code related to auth" <-> "find work related to reducing churn")
- **Markdown as the lingua franca** (code docs <-> business context docs)

The extractors don't just parse `.rs` and `.md` for code structure. They also parse `.oh/` files for business context. Same pipeline, different content.

**The harness IS the CodeState vision, applied to the full agent working context — not just code.**

### Evolved Architecture (from CodeState exploration)

```
git2              <- what changed? (replaces filesystem scanning for repos)
tree-sitter       <- parse code -> symbols
pulldown-cmark    <- parse markdown -> sections (including .oh/ business context)
LanceDB           <- store + search everything (columnar + vectors + full-text)
DuckDB (optional) <- SQL analytics overlay via lance-duckdb extension
MCP server        <- agent interface
```

---

## Repo Structure

```
my-project/
+-- .oh/                          <- the harness context
|   +-- outcomes/
|   |   +-- reduce-churn.md       <- aim + mechanism + signals
|   |   +-- api-reliability.md
|   +-- signals/
|   |   +-- p95-latency.md        <- SLO definition + thresholds
|   |   +-- onboarding-time.md
|   +-- guardrails/
|   |   +-- no-breaking-api.md    <- hard constraint
|   |   +-- require-migration.md
|   +-- metis/
|   |   +-- 2026-03-caching.md    <- "we tried X, learned Y"
|   |   +-- 2026-02-auth-flow.md
|   +-- sessions/
|       +-- current.md            <- active work context
+-- CLAUDE.md                     <- code conventions (existing)
+-- src/                          <- the code
+-- .git/                         <- history of everything above
```

---

## Connection to Artium/Ross Conversations

| Ross's concept | In this harness |
|---|---|
| "Reliability is the business goal" | Outcomes + signals make reliability measurable per project |
| "Evals and guardrails" | Guardrails as repo artifacts with severity levels |
| "Telemetric pricing" | Signals/SLOs provide the transaction-level value measurement |
| "The seed that grows per client" | `.oh/` directory IS the seed — starts minimal, accumulates metis |
| "Earned trust dial" | Guardrail severity starts at hard, relaxes as metis accumulates evidence |
| "Artisan convergence" | Shared outcomes/signals in repo give opinionated engineers shared context |

---

## Minimum Viable Harness

The smallest thing that proves the hypothesis:
1. One outcome file with structured frontmatter
2. One signal file with SLO definition
3. One MCP tool that returns them when queried
4. An agent that demonstrably scopes its work by reading them
5. After a session, one metis file written by the agent, curated by human

## Open Horizons Integration

This aim lives in the OH System context as an Aim under the OH System mission.

The repo-native `.oh/` structure is the *local projection* of the OH graph into a specific codebase. The sync model:
- `.oh/outcomes/` <-> OH Aims for this project
- `.oh/signals/` <-> OH Signals (once that initiative ships)
- `.oh/guardrails/` <-> OH Guardrails for this endeavor
- `.oh/metis/` <-> OH Metis entries for this endeavor

The repo is the working copy. OH is the organizational memory. Git push/pull semantics for sync (not real-time).
