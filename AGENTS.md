# Open Horizons Framework

**The shift:** Action is cheap. Knowing what to do is scarce. We don't build features, we build capabilities.

**The sequence:** aim → problem-space → problem-statement → solution-space → **draft PR** → execute → ship

**Where to start (triggers):**
- Can't explain why you're building this → `/aim`
- Keep hitting the same blockers → `/problem-space`
- Solutions feel forced → `/problem-statement`
- About to start coding → `/solution-space`
- Work is drifting or reversing → `/salvage`

**Reflection skills (use anytime):**
- `/review` - Check alignment before committing
- `/dissent` - Seek contrary evidence before one-way doors
- `/salvage` - Extract learning, restart clean

## Draft PR (between solution-space and execute)

After `/solution-space` produces a recommendation, **create a draft PR before writing code**. The draft PR:
- Names the branch, states the problem, and links the issue
- Summarizes the chosen solution from solution-space analysis
- Becomes the home for all /ship pipeline comments (review, dissent, adversarial test, merit assessment, etc.)
- `/execute` writes code on this branch; `/ship` runs the quality gate on this PR

This ensures every piece of work has a PR home before implementation begins. No orphan branches, no post-hoc PR creation.

## /ship Definition (for this project)

`/ship` = the full quality gate before merge. **12 steps, defined in `.claude/agents/ship.md`.**

Summary: review → dissent → fix → **mark ready** → adversarial test → merit → resolve TODOs → **manual verify → delivery verify** → README → smoke+CI → merge.

Step 3b (mark ready) converts the draft PR to ready-for-review, which triggers smoke tests and CodeRabbit review in CI. Both are gated behind `draft == false` so they skip during the draft phase when code is still being iterated on.

No step is optional. "Merge when green" is not ship. Steps answer different questions — don't collapse them:
- Review/dissent: "Is the code correct?"
- Adversarial test: "What breaks under pressure?"
- Merit assessment: "Does this deliver outcome value?"
- Resolve TODOs: "Is everything accounted for?"
- Manual verification: "Does the computation work with real data?"
- **Delivery verification: "Can an agent actually see this through MCP tools?"** *(see guardrail: computed-but-not-delivered)*

**Key insight:** Enter at the altitude you need. Climb back up when you drift.

## Repo-Native Alignment MCP

This project IS the RNA MCP server. When working here, use its own tools.

**IMPORTANT: Use RNA MCP tools for code exploration, NOT Grep/Read/Bash. This project IS the RNA MCP server — dogfood it.**

| Instead of... | Use RNA MCP |
|---|---|
| `Grep` for symbol names | `search(query, kind, language, file)` |
| `Read` to trace function calls | `search(node, mode: "neighbors")` |
| `Grep` for "who calls X" | `search(node, mode: "impact")` |
| `Read` to find .oh/ artifacts | `search(query, include_artifacts=true)` |
| `Bash` with `grep -rn` | `search(query)` — searches code, artifacts, and markdown |
| Recording learnings/signals | Write to `.oh/metis/`, `.oh/signals/`, `.oh/guardrails/` (YAML frontmatter + markdown) |
| Searching git history | `search(query)` — returns commits; use `git show <hash>` via Bash for diffs |

**If RNA returns empty results — diagnose before falling back:**
- Try a broader query, different `kind`, or no filters first
- Do NOT silently fall back to Grep/Read — that defeats the purpose
- If the index is genuinely stale, say so explicitly rather than substituting file reads

**Every Grep/Read used instead of an RNA tool is a friction event.** Log it in the session's friction log table. A session with 0 friction events and 30 Grep calls isn't frictionless — it's unmonitored.

**MCP Tools:**
1. `search` -- all-in-one: code symbols, .oh/ artifacts, commits, markdown, and graph traversal
2. `outcome_progress` -- structural join for outcome tracking
3. `list_roots` -- workspace root management
4. `repo_map` -- repository orientation (top symbols, hotspots, outcomes, entry points)

**Writing business artifacts:** Write directly to `.oh/` using the Write tool. See `.oh/metis/`, `.oh/signals/`, `.oh/guardrails/` for frontmatter templates.

**Workflow:**
- Before starting work: business context is auto-injected on first tool call
- Explore code: `search("query")` -> `search(query, mode="neighbors")` -> `search(query, mode="impact")`
- After completing work: write learnings to `.oh/metis/<slug>.md`
- When checking progress: call `outcome_progress` with `agent-alignment`
- When discovering constraints: write to `.oh/guardrails/<slug>.md`
- Tag commits with `[outcome:agent-alignment]`

---

# Project Context

## Purpose
MCP server with a workspace-wide context engine. Incrementally scans repos, extracts a multi-language code graph (symbols, topology, schemas, PR history), and makes business outcomes, code structure, markdown, and git history queryable as one system. Agents stay aligned to declared intent because that intent lives in the repo as structured, queryable artifacts.

## Current Aims
- **context-assembly** (active): Agents get the fractal, local knowledge they need for a given task without manual context loading. Mechanism: incremental scanning, pluggable extraction, unified code graph, semantic search, structural joins, auto-injection.
- **agent-alignment** (maintenance): Work stays connected to declared outcomes. Architecture settled, feedback loop exists. Remaining work: bug fixes, tool cleanup, adoption.
- **human-led-curation** (proposed): LLM-assisted corpus curation. Deferred until manual /distill sessions prove insufficient.

## Key Constraints

### Hard guardrails
- **repo-native**: No external store; `.oh/` in the repo, git-versioned. [Details](.oh/guardrails/repo-native.md)
- **lightweight**: Adding an outcome = writing a markdown file. [Details](.oh/guardrails/lightweight.md)
- **git-is-optimization-not-requirement**: Scanner works on any directory; git adds precision when present. [Details](.oh/guardrails/git-is-optimization-not-requirement.md)
- **no-language-conditionals-in-generic**: All per-language behavior goes through LangConfig, never `if language ==` in generic.rs. [Details](.oh/guardrails/no-language-conditionals-in-generic.md)
- **no-parallel-cargo-agents**: One cargo build per target directory; use worktrees for parallel builds. [Details](.oh/guardrails/no-parallel-cargo-agents.md)
- **computed-but-not-delivered**: New metadata must wire through 3 layers — extraction, LanceDB schema, MCP rendering. [Details](.oh/guardrails/computed-but-not-delivered.md)
- **dogfood-rna-tools**: Use RNA's own tools for code exploration; every Grep/Read fallback is a friction event to log. [Details](.oh/guardrails/dogfood-rna-tools.md)

### Soft guardrails
- **extractors-are-pluggable**: Don't hardcode extraction strategy per file type. [Details](.oh/guardrails/extractors-are-pluggable.md)
- **test-with-real-mcp-client**: Test MCP changes with real clients, not just curl. [Details](.oh/guardrails/test-with-real-mcp-client.md)
- **extract-fully-at-parse-time**: Capture all AST-available metadata during extraction; the AST is only available once. [Details](.oh/guardrails/extract-fully-at-parse-time.md)
- **subagent-prompts-require-rna-directive**: Sub-agent prompts must include RNA tool usage requirements. [Details](.oh/guardrails/subagent-prompts-require-rna-directive.md)
- **metis-curation-requires-human-judgment**: LLMs surface candidates; humans judge. No auto-promotion or indiscriminate application. [Details](.oh/guardrails/metis-curation-requires-human-judgment.md)
- **ship-steps-visible-on-pr**: Ship pipeline findings must be posted as PR comments. [Details](.oh/guardrails/ship-steps-visible-on-pr.md)

## Patterns to Follow
- `[outcome:X]` in commit messages to link work to outcomes
- Write to `.oh/` to close the feedback loop (metis, signal, guardrail, outcome) — see existing files for frontmatter format
- Structural joins (outcome_progress) over keyword search for the core use case
- Pluggable extractors: implement `Extractor` trait for new file types (Phase 1: sync). `Enricher` trait for background enrichment (Phase 2: async, e.g., LSP)
- Graph model: `Node` + `Edge` types with `ExtractionSource` provenance and `Confidence` levels
- Source-capable records: wrap in `SourceEnvelope` at the outbox seam for future FEED publishing
- BTreeMap for frontmatter (deterministic output)
- YAML frontmatter + markdown body for all `.oh/` artifacts
- Scanner excludes configurable via `.oh/config.toml`
- Use compiler-driven refactoring (add field, let `cargo check` find every construction site)
- `cargo install --path .` before `/mcp` reconnect (or restart Claude Code)
- Parallel worktree builds: `scripts/prep-worktree.sh <path> <branch>` creates a worktree with warm build cache (hardlinks `target/`). Set `CARGO_TARGET_DIR=$WORKTREE/target` before cargo commands. Enables genuinely parallel builds on M4 Max without cache thrashing.
- **Cargo output: save to file, then grep/tail as needed.** Never pipe cargo commands through `tail` or `grep` directly — you'll miss errors and have to re-run the whole build. Instead: `cargo test 2>&1 > /tmp/cargo-out.txt; echo "exit: $?"` then grep or tail the file. Use unique filenames per agent to avoid conflicts.

## Anti-Patterns to Avoid
- Don't search function bodies in code search (noise) — match name + signature only
- Don't call a union of four greps an "intersection query"
- Don't test MCP with curl alone — protocol negotiation differs from real clients
- Don't port fsPulse's 4-phase scanner wholesale — RNA needs simpler: detect changed → extract → index

## Decision Context
Solo developer. PRs go through the full /ship pipeline (12 steps). "Done" = all TODOs resolved, manually verified with real data, tests pass, MCP client connects. Session learnings recorded as metis via MCP tools.

## Key Modules
- `src/service.rs` — shared service layer (CLI and MCP both delegate here — no capability drift)
- `src/graph/` — unified graph model (types, LanceDB schemas, petgraph index)
- `src/scanner.rs` — incremental file scanner (mtime + git + configurable excludes)
- `src/extract/` — pluggable extractors (Extractor trait + Enricher trait)
- `src/server/` — MCP server (thin adapters to service layer)
- `src/embed.rs` — semantic search (fastembed + LanceDB)
- `src/query.rs` — outcome_progress structural joins

## Shipped Capabilities
- LSP enrichment: 252 `Calls` edges via rust-analyzer callHierarchy (pyright, tsserver, gopls, marksman registered)
- Schema extractors: .proto, SQL, OpenAPI
- Multi-root workspace: `~/.config/rna/roots.toml` + per-root scanning
- PR merge extraction: git merge history → graph nodes + outcome_progress integration
- Graph persistence: LanceDB cache at `.oh/.cache/lance/`, loads in <1s on restart
- Context injection: business context auto-delivered on first tool call
