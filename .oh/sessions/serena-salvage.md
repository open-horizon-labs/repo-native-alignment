# Salvage: Serena

## Source
`~/src/serena` — Python coding agent toolkit. 44+ MCP tools, 30+ languages via unified LSP abstraction (`solidlsp`). Symbol-aware code editing, memory system, multi-project queries. Published as `mcp-server-serena`.

## Aim Filter
RNA is read/align infrastructure — it serves agents but doesn't act as one. Serena is a coding agent toolkit with editing capabilities. Only salvage what strengthens RNA's code navigation, LSP integration, or agent ergonomics. Editing tools are out of scope.

---

## LOW-BAR — Obvious, ought to do

### 1. Name-path symbol matching
Serena's `find_symbol` accepts hierarchical symbol paths like `MyClass/my_method[0]` for overloaded methods, `auth/handlers/validate` for nested namespaces. RNA's `search` matches on name only — no path hierarchy.
- **RNA approach:** Accept path separators in `query` or `node` params. Parse `Foo/bar` as "symbol `bar` within `Foo`", filter nodes matching both parent and child name.
- **Effort:** Small — filter logic in flat search + graph traversal
- **Payoff:** Agents working in Java/TypeScript/C# (heavily namespaced) can be precise without knowing full stable IDs

### 2. Synchronous LSP wrapper pattern
Serena's `solidlsp` is a synchronous wrapper over async LSP because asyncio deadlocks were a real problem in long-running tool interactions. RNA's LSP enricher is already synchronous in the foreground path — but the async/sync boundary is still causing issues (concurrent write races, background enrichment conflicts). Serena solved this by making the API contract synchronous from the start.
- **RNA approach:** Review the LSP enrichment path for any remaining async/sync mismatches. The mutex fix in #357 is a band-aid; the right fix may be fully synchronizing the enrichment pipeline.
- **Effort:** Medium — architectural review
- **Payoff:** Eliminates the class of concurrent-persist bugs we've been fighting

### 3. Multi-project symbol query
Serena's `query_project` lets agents execute read-only tools against other registered projects without switching context. RNA has multi-root (scanning multiple paths into one graph) but not "query a different project while staying in this one."
- **RNA approach:** `search(root="all")` already exists. Extend to accept a slug for a separately registered project root. Or expose a `list_queryable_projects` + `query_project` pattern at the MCP level.
- **Effort:** Small — multi-root infrastructure already exists
- **Payoff:** Agents working across microservices or monorepos can stay in one context. Directly enables the domain-context-compiler outcome — this is how agents connect infrastructure config repos to code repos without merging them into a single index.

---

## HIGH-BAR — Innovative, fits our aims

### 1. Symbol children/descendants with configurable depth
Serena's `get_symbols_overview(file, depth=2)` returns a file's entire symbol hierarchy — classes with their methods, functions with their nested functions — at configurable depth. RNA returns flat symbol lists. Agents wanting "show me the structure of this module" make N calls (one per level) or use `compact: true` on a single flat result.
- **RNA approach:** Add `depth` parameter to traversal modes. `search(node="MyModule:module", mode="neighbors", depth=2, edge_types=["defines"])` would return the module + all its members + their members.
- **Effort:** Medium — extend traversal to walk Defines edges N levels
- **Payoff:** "Show me the structure of the server module" in one call. Dramatically reduces agent context burn on orientation tasks.

### 2. Token counting / efficiency tracking per-query
Serena's `ToolUsageStats` tracks input/output tokens per tool call across a session. RNA has no per-query token accounting — agents can't tell which RNA queries are cheap (compact symbol lookup) vs expensive (full impact traversal on a high-connectivity node). This matters for prompt budget management.
- **RNA approach:** Return token estimates in the index footer. The index line already shows `9,551 symbols · schema v11` — adding `~2.4K tokens` would let agents budget context usage.
- **Effort:** Small — count chars in result, report rough estimate
- **Payoff:** Agents can choose `compact: true` deliberately when they know context is tight, rather than discovering it empirically

---

## MID-BAR — Inventory, probably won't do

| Feature | What Serena does | Why skip |
|---------|----------------|----------|
| Symbol-aware editing (`replace_symbol_body`, `insert_before/after_symbol`, `rename_symbol`) | Edit code at symbol level, not line level | RNA is read-only by design — agents have their own editors |
| Memory system (markdown in `.serena/memories/`) | Persistent project knowledge across sessions | RNA's `.oh/metis/` serves this; Serena's memory is agent-facing, RNA's is analyst-facing. Different UX. |
| Workflow modes (planning, editing, interactive, one-shot) | Pre-configured tool sets per agent workflow | RNA doesn't control the agent's workflow; it provides context |
| GUI/dashboard | Web-based logging and monitoring | RNA's log noise fixes (lance=warn) and structured output cover this |
| Shell execution (`execute_shell_command`) | Unrestricted bash | Too permissive; not RNA's scope |
| Prompt factory (Jinja templates) | User-editable prompts as YAML | RNA uses agents which have their own prompting |
| JetBrains backend | IDE-backed symbol analysis instead of LSP | RNA's LSP enrichment is the right layer; IDE backends add coupling to specific IDEs |
| Token counting via Anthropic API | Exact token counts by calling Claude | Network dependency; RNA is offline-first |

---

## Key Architectural Insight

Serena is a coding **agent** — it acts on code (edits, renames, shell). RNA is code **context** — it answers questions about code. The salvage is narrow: the navigation and discovery patterns from Serena that apply equally to read-only queries.

The most interesting insight from Serena is the **synchronous LSP wrapper pattern**. Serena built `solidlsp` specifically because async LSP caused deadlocks in long-running agent sessions — exactly the class of problem RNA has been hitting in the concurrent persist/enrich path. Their solution (synchronous contract from the start) is architecturally sounder than adding mutexes after the fact.

The second insight is **depth-aware hierarchy traversal**. RNA currently exposes a flat symbol list and a graph traversal. Serena's experience shows agents frequently need "the structure of this module at depth N" — a query that sits between the two. Adding a `depth` parameter to Defines-edge traversal would close this gap.
