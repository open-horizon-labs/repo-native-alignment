# Graph-Augmented Context for Coding Agents: Hybrid Vector-Graph Representations for Codebase Understanding

## Abstract

A software repository is not a collection of text files. It is a graph of relationships between symbols, modules, services, and business outcomes. Traditional AI coding tools flatten this structure into text and rely on language models to rediscover relationships through token matching. We take the opposite approach: extract the repository's native structure and expose it directly as a queryable graph, augmented with semantic embeddings. Agents retrieve structured subgraphs instead of text blobs, allowing them to reason about the system in the same structural terms that the compiler and runtime use. Using RNA, an early-stage local context discovery tool, we benchmark against standard Claude Code tooling on 5 realistic developer questions against a [25K-line Rust codebase](https://github.com/open-horizon-labs/unified-hifi-control) (N=5 per condition). Results: 60% lower cost ($0.65 vs $1.62), 72% faster (146s vs 530s), +5% quality (4.89 vs 4.67/5.0), with the strongest gains in multi-hop traversal and complexity analysis. The primary value is efficiency, not accuracy — agents get equivalent answers faster and cheaper.

## 1. Introduction

### 1.1 Complex Systems Are Networks, Not Collections

Complex systems — software, organizations, markets, biology — share a structural property: **relationships carry more information than components**. Consider the same four components arranged two ways:

```
A → B → C → D       (pipeline)
A → B, A → C, A → D (hub)
```

Same nodes. Completely different behavior. The difference is the edges.

This principle has a name in complex systems theory: **structure carries more information than components**. A city is not a list of buildings — it is a network of roads connecting neighborhoods to services. A protein is not a sequence of amino acids — it is a folded structure of interaction surfaces. A codebase is not a collection of files — it is a dense dependency graph where every component is constrained by its relationships to others.

When we index a 25K-line Rust codebase with RNA, we find:

```
9,366 nodes (functions, types, modules, tests)
16,364 edges (calls, references, defines, implements, depends_on)
```

That is ~1.7 edges per node — enough connectivity that the meaning of each node comes primarily from its relationships. An agent that reads only the node (a function's source code) misses the structural context that determines its actual behavior: who calls it, what depends on it, what breaks if it changes.

### 1.2 The Flattening Problem

Current coding agent harnesses (Claude Code, Codex, OpenCode) provide text-based tools: grep for content search, file reading for comprehension, glob for file discovery. These tools treat code as text. The implicit pipeline is:

```
graph-structured system → flattened text → LLM reasoning
```

Information is lost in the flattening. When a graph is serialized into text, you lose dependency direction, distance between components, cluster boundaries, and transitive relationships. An agent reading file chunks might see `A calls B` and `B calls C` in separate tool calls but never synthesize `A → C` — a relationship the graph makes explicit.

Text search can answer "where is function X?" but fails at structural questions:

- "What transitively depends on X?" (impact analysis)
- "What calls X through intermediaries?" (call chain tracing)
- "Which functions have the highest cyclomatic complexity?" (computed metrics)
- "What tests cover this production code?" (test-to-code mapping)

These are graph questions. Answering them through sequential grep-read-grep loops is expensive: each hop in a call chain costs a tool call, context tokens, and wall time. Pre-computing the graph collapses multi-hop exploration into single queries.

### 1.3 The Context Compiler Pattern

RNA implements what we call a **context compiler**: a pipeline that transforms raw artifacts into structured, queryable context for LLM reasoning.

```
Raw artifacts (code, docs, git history)
        ↓
Entity extraction (tree-sitter: symbols, types, modules)
        ↓
Relationship extraction (LSP: calls, references, implements)
        ↓
Knowledge graph (nodes + typed edges)
        ↓               ↓
Graph queries      Embeddings
(structure)        (semantics)
        ↓               ↓
     Context assembly
        ↓
     LLM reasoning
```

The two retrieval paths are complementary: **graph = structure** (what connects to what, what depends on what), **embeddings = meaning** (what is conceptually related, what answers this question). Together they approximate how humans reason about complex systems — we navigate both structural relationships ("this function calls that service") and semantic associations ("this is related to authentication") simultaneously. Neither alone is sufficient: graph without semantics can't answer "find code related to payment processing"; embeddings without graph can't answer "what breaks if I change this."

Compare with traditional RAG:

```
documents → chunk → embed → similarity search → LLM
```

The difference is structure preservation. Traditional RAG discards the relationships between chunks. The context compiler preserves them, so the model reasons over topology — `payment_api → order_service → inventory_db` — not scattered file contents.

### 1.4 Two Layers of Context

Context for a coding agent comes from two sources:

**Prompt context** — what's assembled into the system prompt before the model reasons: instructions, skill definitions, MCP tool descriptions, prior conversation turns. This is curated at the harness level and determines behavior and capabilities.

**Local context** — what's discoverable in the working environment: code, documentation, configuration, git history, business artifacts. This is queried during the task via tool use. Local context is orders of magnitude larger than prompt context (a codebase is millions of tokens; a system prompt is thousands) and must be accessed selectively.

These layers interact. Prompt context tells the agent *how* to explore; local context is *what* it explores. RNA addresses the local context layer: making the structural knowledge in a codebase queryable so the agent retrieves precisely the relevant subgraph, not everything that matches a keyword.

### 1.5 Three Layers of Codebase Knowledge

A codebase encodes three layers of knowledge:

1. **Structural knowledge** — what exists and how it relates. Functions call functions, structs contain fields, modules import modules. This is a directed graph recoverable through static analysis and LSP queries.

2. **Semantic knowledge** — what concepts are represented and how they cluster. "Authentication" spans multiple files; "payment processing" may not appear as a literal string. This requires vector representations.

3. **Intentional knowledge** — why things exist, what outcomes they serve, what constraints govern them. This lives in documentation, commit messages, and business artifacts. It requires both vector search (discovery) and graph structure (tracing intent to implementation).

Text search accesses layer 1 for single-hop queries but fails for multi-hop. It cannot access layer 2 at all. It accesses layer 3 only through keyword matching, which is fragile.

### 1.6 Hypothesis

A hybrid vector-graph representation that pre-computes structural relationships (via static analysis and LSP) and semantic embeddings (via transformer models) will enable coding agents to:

- **Use fewer tokens** to answer the same questions (fewer tool calls, less irrelevant context)
- **Achieve equal or better answer quality** (more complete, more specific, fewer factual errors)
- **Access information text search cannot produce** (transitive dependencies, complexity metrics, test coverage gaps)

### 1.7 Generalization Beyond Code

This hypothesis generalizes beyond codebases. Any complex system where relationships dominate — where the meaning of a component comes primarily from its connections — benefits from the same approach:

| Domain | Objects (easy to describe in text) | Relationships (where the information lives) |
|--------|-----------------------------------|---------------------------------------------|
| Software | functions, classes | call graph, dependency graph |
| Organizations | teams, roles | communication graph, reporting structure |
| Markets | assets, instruments | correlation graph, exposure chains |
| Biology | proteins, genes | interaction networks, metabolic pathways |
| Legal | statutes, cases | citation graph, precedent chains |
| Infrastructure | services, databases | dependency graph, failure propagation |

In each domain, listing the objects tells you almost nothing about the system. Describing the connections reveals its structure. We focus on codebases because they provide the tightest feedback loop: tool calls are logged, answers are verifiable against source code, and ground truth is available for evaluation.

## 2. System Design: RNA

### 2.1 Architecture

RNA (Repo-Native Alignment) is a local context discovery tool for coding agents. It provides:

- **Incremental scanning** — mtime-based file change detection with git diff acceleration
- **Multi-language extraction** — tree-sitter parsing across 22 languages producing typed graph nodes (functions, structs, traits, enums, imports, etc.)
- **LSP enrichment** — call hierarchy and reference queries from language servers (rust-analyzer, pyright, tsserver, gopls) adding Calls and ReferencedBy edges
- **Semantic embedding** — MiniLM-L6-v2 embeddings with optional Jina cross-encoder reranking
- **Unified storage** — LanceDB for both graph persistence and vector search
- **4 MCP tools** — `search` (code symbols, artifacts, markdown, graph traversal in one call), `outcome_progress`, `repo_map`, `list_roots`

### 2.2 Graph Model

Nodes represent code symbols with metadata:
- Identity: name, kind, file, line range, language
- Metrics: cyclomatic complexity, PageRank importance
- Provenance: extraction source (tree-sitter, LSP, git)

Edges represent typed relationships:
- **Calls** — function A calls function B (from LSP callHierarchy)
- **ReferencedBy** — type A is referenced in file B (from LSP textDocument/references)
- **Defines** — module A defines function B (from tree-sitter scope analysis)
- **HasField** — struct A has field B (from tree-sitter)
- **DependsOn** — symbol A depends on symbol B (from import resolution)
- **Implements** — struct A implements trait B (from LSP typeHierarchy)
- **References** — markdown A links to markdown B (from pulldown-cmark)

### 2.3 Query Modes

The `search` tool supports:
- **Flat search** — ranked by embedding similarity (hybrid BM25+vector), with name-match fallback
- **Neighbors** — direct connections from a node (outgoing, incoming, or both)
- **Impact** — reverse transitive closure (what depends on X?), filtered to Calls+ReferencedBy edges
- **Reachable** — forward transitive closure (what does X depend on?)
- **Tests_for** — which test functions transitively call this symbol?

### 2.4 Maturity Caveats

RNA is early-stage software. Known limitations at time of benchmarking:

- LSP enrichment requires language servers on PATH and proper workspace initialization
- Embedding index rebuild takes 60-90s on first use; cached thereafter
- CLI graph traversal entry point resolution depends on embedding index quality
- Cross-language edges exist only where LSP servers support it
- The tool was developed and primarily tested on its own codebase (single project, Rust-only)

## 3. Benchmark Design

### 3.1 Task Selection

We designed 5 questions representing realistic developer activities during a code exploration session:

| Q | Developer Question | What It Tests |
|---|---|---|
| Q1 | "I need to add a volume step override. What code path do I need to understand?" | Multi-hop call chain tracing |
| Q2 | "I'm adding a new adapter. What do I need to implement?" | Codebase orientation, pattern discovery |
| Q3 | "The SOAP parsing is buggy. What tests cover it?" | Test coverage analysis |
| Q4 | "I want to refactor the Zone struct. What breaks?" | Blast radius / impact analysis |
| Q5 | "Which adapter has the most complex control flow?" | Complexity ranking, tech debt assessment |

Questions were designed to be:
- **Realistic** — every developer asks these when joining a project or planning changes
- **Verifiable** — answers can be checked against source code
- **Multi-tool** — no single grep command answers any of them completely
- **Varied** — spanning RNA's identified strengths and areas where grep is adequate

### 3.2 Target Codebase

[unified-hifi-control](https://github.com/open-horizon-labs/unified-hifi-control): a 25K-line Rust application with 5 adapters (Roon, LMS, OpenHome, UPnP, HQPlayer), an event bus architecture, web UI (Dioxus), and MCP server. The codebase is authored by the benchmark designer — noted as a threat to validity in section 3.6.

Graph statistics after RNA indexing:
- 15,132 nodes (code symbols + markdown sections)
- 23,576 tree-sitter edges + 5,067 LSP call edges = 28,643 total
- Embedding index: ~5,000 vectors (embeddable symbols)

### 3.3 Agents

**Vanilla agent**: Standard Claude Code tooling — Grep, Read, Glob, Bash. No MCP servers. Instructed to use only these tools.

**RNA agent**: Same base tools + RNA MCP server providing `search` and `repo_map` tools natively. The MCP server is configured in the target repo's `.mcp.json`, pre-indexed with LSP call edges. Prompt includes suggested MCP tool calls for each question (e.g., `search(mode: "impact", hops: 3)`).

Both agents use the same model (Claude Opus 4.6, 1M context), same token budget, same question prompt (minus tool-specific instructions). Both run via `claude --print` from the target repository directory.

### 3.4 Measurement

| Metric | Source |
|---|---|
| Total tokens (input + output) | Agent SDK usage report |
| Wall time | Agent SDK duration_ms |
| Tool calls by type | Parsed from transcript |
| Solution quality (5 criteria, 1-5 each) | Independent scorer agent using rubric |
| Functions/files named | Counted from solution text |
| Factual errors | Verified against source code |
| Max hop depth | Deepest call chain traced |

### 3.5 Evaluation

Each solution scored independently (not head-to-head) by a scorer agent using a rubric with anchored examples at 1/3/5. Criteria weighted by task importance:

- Q1 Volume flow trace: 25% (multi-hop, hardest for grep)
- Q2 Adapter checklist: 20%
- Q3 Test coverage: 20%
- Q4 Zone blast radius: 20%
- Q5 Complexity ranking: 15%

### 3.6 Caveats and Threats to Validity

**Tool nudging**: The RNA agent's prompt includes explicit CLI commands to use. The vanilla agent receives no equivalent "use grep like this" guidance. This creates an asymmetry — the RNA agent is told HOW to use its tools, while the vanilla agent must figure it out. We chose this design because: (a) without explicit commands, agents default to Grep/Read even when RNA is available (observed in pilot runs), and (b) in practice, RNA users would have AGENTS.md guidance suggesting tool usage patterns.

**Harness limitations**: RNA is accessed via native MCP integration (`claude --print` from the target repo directory with RNA configured in `.mcp.json`). The MCP tool schemas add ~22K tokens to the system prompt (4 tools with full JSON parameter schemas). This is a one-time cache_creation cost per session, but the cached prefix is re-read on every turn. RNA MCP agents also tend to use more turns (38 vs 6 in pilot runs) because graph traversal queries are separate tool calls, further amplifying cache_read totals. This overhead is reducible — the current parameter descriptions are verbose and could be slimmed significantly (#295).

**Single codebase**: Results are from one Rust project. Generalization to other languages, project sizes, and architectures is not established.

**Single model**: All runs use Claude Opus 4.6. Results may differ with other models or model versions.

**Scorer bias**: The scorer is also an LLM, subject to its own biases. We mitigate by scoring independently (not comparatively) and using anchored rubric examples.

**Small sample**: N=5 per condition. Statistical significance is limited. Results should be interpreted as directional evidence, not definitive proof.

**Developer of RNA conducted the benchmark**: The benchmark was designed and run by the RNA developer. Independent replication would strengthen the findings.

## 4. Results

### 4.1 Efficiency Metrics

| Metric | Vanilla (N=4) | RNA MCP (N=5) | Delta |
|---|---|---|---|
| Cost (USD) | $1.62 ($1.52–$1.71) | $0.65 ($0.56–$0.72) | **-60%** |
| Wall time (s) | 530 (371–623) | 146 (133–163) | **-72%** |

### 4.2 Quality Scores (1-5 scale, scored independently by LLM evaluator)

| Criterion (weight) | Vanilla | RNA MCP | Delta |
|---|---|---|---|
| Q1 Volume flow (25%) | 4.50 (4–5) | 5.00 (5–5) | +0.50 |
| Q2 Adapter checklist (20%) | 5.00 (5–5) | 5.00 (5–5) | +0.00 |
| Q3 Test coverage (20%) | 5.00 (5–5) | 5.00 (5–5) | +0.00 |
| Q4 Zone blast radius (20%) | 4.75 (4–5) | 4.60 (4–5) | -0.15 |
| Q5 Complexity ranking (15%) | 4.00 (3–5) | 4.80 (4–5) | +0.80 |
| **Weighted total** | **4.67** (4.25–5.0) | **4.89** (4.65–5.0) | **+0.22** |

### 4.3 Specificity Metrics

| Metric | Vanilla | RNA MCP | Delta |
|---|---|---|---|
| Functions/files named | 45.5 (42–53) | 57.2 (47–63) | **+26%** |
| Factual errors | 2.5 (1–5) | 1.6 (0–3) | **-36%** |

### 4.4 Observations

**Q1 (volume flow):** Both agents trace the full chain from UI to device. RNA consistently scores 5.0 because graph traversal returns the complete path in structured form; vanilla occasionally misses intermediate hops, scoring 4.5.

**Q2 (adapter checklist):** Both score 5.0. This question has a well-defined answer (two traits, one macro, specific files) that both approaches find equally well via search or grep.

**Q3 (test coverage):** Both score 5.0. RNA uses `tests_for` mode systematically; vanilla greps for function names in test files. Both arrive at the same conclusion (zero unit tests on SOAP parsing).

**Q4 (blast radius):** Near-identical scores (4.75 vs 4.60). Vanilla traces slightly deeper because sequential file reading discovers incidental usages. RNA returns the structural dependents but may miss runtime-only references.

**Q5 (complexity ranking):** RNA's clearest advantage (+0.80). RNA returns cyclomatic complexity numbers directly via `sort_by: "complexity"` — information grep fundamentally cannot compute. Vanilla must estimate complexity from line count and visual inspection, producing less precise rankings.

## 5. Discussion

### 5.1 Where Graph Augmentation Helps

The strongest efficiency gains come from questions requiring **multi-hop traversal** (Q1: volume flow) and **computed metrics** (Q5: complexity ranking). These are tasks where text search requires sequential exploration — read a file, find a function call, grep for that function, read its file, repeat. Each hop adds a tool call, context tokens, and wall time. Graph traversal collapses this into a single query.

The strongest quality gain (Q5: +0.80) comes from **information text search cannot produce at all**: cyclomatic complexity scores. RNA pre-computes these during extraction and returns them as node metadata. An agent using grep would need to parse ASTs or count branch points manually — impractical within a tool-use loop.

### 5.2 Where Text Search Is Sufficient

For well-defined lookup tasks (Q2: adapter checklist, Q3: test coverage), both approaches score identically (5.0). When the answer is "find these specific traits and list their methods," grep and file reading are perfectly adequate. The information is localized, the search terms are known, and multi-hop traversal adds no value.

Q4 (blast radius) slightly favors vanilla (4.75 vs 4.60). Sequential file reading discovers incidental usages that graph traversal misses — for example, a function that constructs a `Zone` via a helper function, where the helper isn't in the direct `impact` results. Text search's exhaustiveness compensates for its inefficiency in these cases.

### 5.3 The Tool Adoption Problem

A persistent finding throughout RNA's development: agents default to Grep/Read even when graph tools are available and would be more efficient. In pilot benchmarks, RNA agents used Grep 10+ times despite prompts saying "do NOT use Grep." This suggests that:

1. Tool adoption cannot be solved by prompt engineering alone
2. Tool descriptions in system prompts compete with training-time tool preferences
3. The harness (not the model) determines which tools get used

Implications for the field: building better code understanding tools is necessary but not sufficient. The integration surface — how tools are presented to agents, what the system prompt says, whether the agent's training included the tool — may matter more than the tool's capabilities.

### 5.4 Harness Design Assumptions That Limit Tool Effectiveness

Current coding agent harnesses make assumptions that work against graph-augmented context:

**Context-per-interaction vs. persistent queryable state.** MCP tools return text that enters the context window. A graph traversal returning 50 nodes occupies context — and gets compacted away 3 turns later. The agent then re-queries the same information. The graph IS persistent state, but the protocol forces it through an ephemeral text channel. A better design: tools that maintain queryable state the agent can re-access without re-paying the context cost.

**KV-prefix cache invalidation.** LLM inference optimizes for stable context prefixes via KV caching. Every MCP tool response is unique content that invalidates the prefix cache. A system prompt with 4 MCP tool schemas (~2K tokens) is cached on the first turn, but the tool results that follow are never cacheable. This means graph-augmented agents pay a higher per-turn inference cost than agents using built-in tools whose outputs are more predictable.

**Context compaction discards tool results.** As the context window fills, harnesses auto-compress older messages. The first thing discarded is typically tool results — exactly the context the agent paid tokens to acquire. An agent that used RNA to discover 20 relevant functions loses that discovery after compaction and must re-discover it. Text search agents suffer the same problem, but because their per-query results are smaller (one grep result vs. a graph traversal), the waste is proportionally less visible.

**Implication:** The question is not "what harness design lets graph tools deliver their full value?" but rather "what harness design gets more value out of LLM models?" The context window is the LLM's working memory. Every token spent on irrelevant grep results, re-discovered facts, or compacted-then-re-fetched tool outputs is a token not spent on reasoning. Graph-augmented context is one approach to this problem — deliver precisely the relevant structural information instead of raw text the model must assemble into structure itself. But the harness must also stop undoing that work through compaction, cache invalidation, and ephemeral tool results. Current harnesses treat context as disposable. The alternative: context as a managed resource, with persistence, incremental updates, and compaction-aware prioritization. Our benchmark measures what's achievable within current harness constraints, not the theoretical ceiling.

### 5.5 Limitations of the Current Approach

- **Cold start cost**: First scan + embedding takes 60-90s. Text search has zero cold start.
- **LSP reliability**: Language server enrichment is fragile — server warm-up timing, workspace configuration, and memory constraints all affect edge quality.
- **Single-codebase validation**: The tool was primarily tested on its own codebase, creating a dogfooding bias that masked issues with other projects.

## 6. Related Work

### 6.1 Context Window Degradation

- **Chroma "Context Rot"** (Hong, Troynikov, Huber, 2025; [paper](https://research.trychroma.com/context-rot), [code](https://github.com/chroma-core/context-rot)) — Tested 18 models including GPT-4.1, Claude 4, and Gemini 2.5 on retrieval tasks with varying context lengths. Found performance degrades at every context length increment, not just near limits. Critically, identified "distractor interference" — irrelevant context actively hurts reasoning, not just dilutes it. Models performed better with shuffled, incoherent haystacks than logically structured ones, suggesting that coherent-but-irrelevant context is the worst case. This is direct empirical evidence that cumulative context (grep results, file reads, prior tool outputs) is worse than selective context. Our approach — graph queries that return only structural relationships — directly addresses distractor interference by not putting irrelevant code in context.

- **LongMemEval** (Wu et al., 2024; [arxiv](https://arxiv.org/abs/2410.10813); ICLR 2025) — Benchmarks 500 curated questions testing five core memory abilities in chat assistants. Found 30% accuracy drops as interaction history lengthens. Proposes session decomposition, fact-augmented key expansion, and time-aware query expansion. Directly relevant to our finding that context compaction discards tool results the agent later needs, forcing re-discovery. Graph-augmented context offers an alternative: the graph is persistent state outside the context window, queryable on demand without accumulating history.

- **AMA-Bench** (Zhao et al., 2026; [arxiv](https://arxiv.org/abs/2602.22769)) — Found existing memory systems fail because they "lack causality and objective information." Similarity-based retrieval is lossy — it finds related content but not the causal chain between concepts. Proposes AMA-Agent with a causality graph and tool-augmented retrieval, achieving 57% accuracy vs. 46% for prior systems. This aligns directly with our hybrid approach: vector embeddings for discovery (find related code), graph edges for causality (trace the call chain, follow the dependency). The parallel is striking — they arrived at "causal graph over similarity retrieval" for agent memory; we arrived at the same conclusion for code understanding.

### 6.2 Code Understanding Tools

- **Code-Graph-RAG** — Neo4j + tree-sitter + UniXcoder embeddings. Similar graph-first approach but requires Docker/Memgraph, no LSP enrichment.
- **CodeGraphContext** — SCIP-based indexing with KuzuDB/Neo4j. Compiler-grade edges but requires build-time indexers.
- **codeTree** — SQLite-based AST analysis with 23 MCP tools. Breadth over depth, no embeddings or LSP.
- **Aider repo-map** — Tag-based code summarization for context window management. Complementary approach (summarization vs. queryable graph).

### 6.3 Agent Architecture

- **12-Factor Agents / Context Engineering** (Dex Horthy; [talk](https://www.youtube.com/watch?v=IS_y40zY-hc), [post](https://github.com/humanlayer/12-factor-agents)) — Framework for systematic context management. Distinguishes harness engineering (tool configuration) from context engineering (what information reaches the model). RNA implements the "give agents the right context" principle via queryable pre-indexed artifacts.
- **Harness Engineering** (Viv Trivedy; [post 1](https://www.vtrivedy.com/posts/claude-code-sdk-haas-harness-as-a-service), [post 2](https://blog.langchain.com/the-anatomy-of-an-agent-harness/)) — Identifies four customization levers: system prompt, tools/MCPs, context, sub-agents. Our benchmark measures the tools/MCPs lever specifically — does a better code exploration tool improve outcomes?
- **Skill Issue: Harness Engineering for Coding Agents** (HumanLayer; [post](https://www.humanlayer.dev/blog/skill-issue-harness-engineering-for-coding-agents)) — Practical guide to harness configuration. Notes that sub-agents function as "context firewalls" and that tool count matters for context budget. RNA's consolidation from 20+ tools to 4 aligns with their finding that fewer, focused tools outperform broad tool suites.

## 7. Conclusion

Complex systems are networks of constraints, not collections of objects. A codebase with 9,366 nodes and 16,364 edges is not a folder of source files — it is a dense dependency network where the behavior of each component is determined primarily by its relationships. Current coding agent harnesses flatten this graph into text and ask language models to rediscover structure through sequential token matching. This works, but it is expensive: each hop in a dependency chain costs a tool call, context tokens, and wall time.

A hybrid vector-graph representation that preserves the repository's native structure enables coding agents to answer developer questions 60% cheaper and 72% faster than text search alone, with comparable or slightly better answer quality (+5% weighted score, +26% specificity, -36% factual errors). The efficiency gain comes from collapsing sequential grep-read-grep exploration loops into single graph queries. The quality gain is concentrated in tasks requiring computed metrics (cyclomatic complexity) and multi-hop traversal (call chain tracing) — information that text search fundamentally cannot produce.

The quality advantage is modest (4.89 vs 4.67 on a 5-point scale). For well-defined lookup tasks, text search is equally effective. The primary value proposition is **efficiency, not accuracy** — the agent gets the same answer faster and cheaper.

Four findings extend beyond the specific tool evaluated:

1. **Structure carries more information than components.** The ~1.7 edges per node in our benchmark codebase means that reading a function's source (the node) without its call graph, dependents, and test coverage (the edges) misses most of the information needed for structural reasoning. Consistent with Chroma's Context Rot findings, returning precisely the relevant structural relationships outperforms returning all text matches even when the text matches contain the answer.

2. **The context compiler pattern generalizes.** Any domain where relationships dominate — legal corpora (citation graphs), organizations (communication graphs), infrastructure (dependency graphs), biology (interaction networks) — can benefit from the same pipeline: entity extraction → relationship extraction → graph + embeddings → structured retrieval. The scaling limit for AI agents operating on complex systems is not context window size but structure extraction.

3. **Tool adoption cannot be prompt-engineered.** Agents default to built-in text search even when explicitly instructed to use graph tools. The integration surface (how tools appear in the system prompt, whether training included the tool) determines adoption more than tool capabilities.

4. **Harness design limits tool effectiveness.** MCP tool schemas consume 22K tokens of system prompt, context compaction discards tool results the agent later needs, and KV-prefix caching is invalidated by every unique tool response. A harness designed for persistent queryable state rather than ephemeral text responses would change the economics significantly.

A natural next step is automatic subsystem detection. The graph's edge density likely implies clear cluster boundaries — modules that are densely connected internally but sparsely connected to other modules. Detecting these automatically would let agents reason about subsystems rather than individual files, which is closer to how engineers actually think about codebases.

These results are from an early-stage tool (RNA v0.1.9) on a single codebase (25K lines Rust) with a small sample (N=5). Independent replication on diverse codebases, languages, and model families would strengthen the findings. The benchmark design, prompts, scoring rubric, and raw results are published alongside this paper for reproducibility.

## Appendix: Benchmark Materials

All benchmark materials are in [`benchmark/`](../benchmark/):

### Prompts
- [Vanilla prompt](../benchmark/prompt-v3.md) — the 5 developer questions without tool guidance
- [RNA MCP prompt](../benchmark/prompt-v3-rna.md) — same questions with suggested MCP tool calls
- [Evaluation rubric](../benchmark/eval-rubric-v3.md) — scoring criteria with anchored 1/3/5 examples

### Scripts
- [run-v3.sh](../benchmark/run-v3.sh) — serial benchmark runner (vanilla + RNA CLI pairs)
- [score-results.sh](../benchmark/score-results.sh) — independent scorer for each solution
- [aggregate-scores.py](../benchmark/aggregate-scores.py) — aggregates scores and efficiency metrics
- [parse-results.py](../benchmark/parse-results.py) — extracts tool call breakdowns from transcripts

### Raw Results
- [Vanilla runs](../benchmark/results/vanilla/) — `v3-run-{01..05}.json` (claude --print output with usage, cost, result)
- [RNA MCP runs](../benchmark/results/rna-mcp/) — `v3-run-{01..05}.json`
- [Scores](../benchmark/results/scores/) — `{vanilla,rna-mcp}-v3-run-{01..05}.json` (per-solution quality scores)
