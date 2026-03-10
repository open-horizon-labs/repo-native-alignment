# oh-evals: Conversational Eval Builder

**Date:** 2026-03-07
**Status:** Plan — not yet started
**Repo:** `open-horizon-labs/oh-evals` (to be created)

## Aim

Execs define what success looks like in conversation. The system turns that conversation into running evals against real systems. Evidence of reliability is generated automatically, in the exec's language.

## The Pattern

Conversation is the source of truth. The compiled runnable is a regenerable cache.

```
Exec conversation              Compiled snapshot (cache)
"Claims per hour should   ──▶  SELECT COUNT(*) FROM fact_claims
 be above 30. Data's in        WHERE processed_at > ...
 BigQuery."
        ↑
   source of truth              regenerable from conversation
```

The exec never sees SQL, Python, or YAML. They see: "Claims per hour: 31 (threshold: 30)"

## How It Works

1. **Conversation** — artisan/agent talks to exec about what success looks like
2. **Discovery** — system probes available data sources (BigQuery, Grafana, Datadog, approval queues) via MCP adapters
3. **Proposal** — system proposes eval in natural language, tests it live against real data
4. **Approval** — exec confirms: "yes, that's what I meant"
5. **Storage** — natural language spec (truth) + compiled runnable (cache) + approval provenance
6. **Continuous run** — eval runs on schedule, reports in exec's language
7. **Reconversation** — when something breaks or changes, resurface the conversation, not the code

## What This Is

An MCP-powered conversational eval builder + runner.

### MCP Tools
- `define_signal` — start the conversation about what to measure
- `test_signal` — run a candidate eval against real data, show results
- `approve_signal` — exec approves, system stores and schedules
- `list_signals` — what are we tracking?
- `run_signals` — execute all approved evals, produce report
- `signal_report` — results in exec-readable format

### Data Source Adapters
Each is an MCP tool or integration:
- BigQuery / Snowflake / Redshift (warehouse queries)
- Grafana / Datadog (metrics)
- Approval queues (human-in-loop tracking)
- Git history (RNA provides this already)
- Agentic eval harnesses (run agent, judge output)

### The `.oh/signals/` File (Updated Model)

```markdown
# Claims Per Hour

Processing throughput for the claims agent should exceed 30 claims per hour
during business hours. Data source: BigQuery `fact_claims` table, filtered
to `agent_processed = true`.

Approved by: Jane (VP Claims), 2026-03-15
Last tested: 2026-03-15 (result: 31, pass)
```

Readable by an exec. The compiled query is a cached artifact the exec never sees.

## Relationship to Other Systems

```
RNA MCP                     oh-evals                    Runtime
- outcomes (what)           - signal specs (measure)    - BigQuery, Grafana
- guardrails (constrain)    - eval runner (execute)     - Datadog, queues
- metis (learn)             - reporter (evidence)       - agent systems
- structural joins          - conversation builder
                                    │
                            OH MCP (organizational)
                            - stores observations
                            - cross-project signals
                            - decision logs
```

RNA defines what matters. oh-evals proves it's working. OH remembers across engagements.

## The Artium Pitch

"We sit with your team. We ask: what does success look like? We turn that conversation into live, running evals against your actual systems. Every week you see a report: here's what the agents delivered, here's where guardrails fired, here's the value per transaction. If you want to change what success means, we have that conversation again."

## Eval Types (All Conversationally Defined)

- **Throughput** — "Are we processing enough?" → warehouse query
- **Accuracy** — "Is the agent getting it right?" → agentic eval with rubric
- **Compliance** — "Are guardrails holding?" → log query for violations
- **Latency** — "Is it fast enough?" → metrics query
- **Human escalation rate** — "Is the approval queue manageable?" → queue metrics
- **Value per transaction** — "What's each action worth?" → derived from throughput + accuracy

## Key Design Decisions

- **No YAML.** LLMs speak English. Execs speak English. The spec is natural language.
- **Compiled form is a cache.** Regenerable from the conversation. Not hand-edited.
- **Separate repo from RNA.** Different concern: RNA = definitions + structural joins. oh-evals = evidence + runtime.
- **MCP-native.** Agents can define, test, and run evals. Execs interact through the agent.
- **Approval provenance.** Every eval traces back to who approved it and when.

## Next Steps (When Ready)

1. Create `open-horizon-labs/oh-evals` repo
2. Scaffold MCP server with `define_signal` and `test_signal` tools
3. Build one adapter (BigQuery or Grafana) as proof of concept
4. Test: conversationally define a signal, compile to query, run against real data
5. Demo to Ross: "Here's what the agent delivered this week, measured automatically"
