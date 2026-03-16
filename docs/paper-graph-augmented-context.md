# Graph-Augmented Context for Coding Agents: Hybrid Vector-Graph Representations for Codebase Understanding

## Abstract

Codebases are graphs of relationships between concepts, responsibilities, and runtime behaviors. When coding agents explore code using text search alone (grep, file reading), they recover these relationships through repeated sequential queries — an expensive process that scales poorly with codebase size and relationship depth. We hypothesize that a hybrid vector-graph representation of a codebase — combining semantic embeddings for concept discovery with a structural graph for relationship traversal — enables coding agents to answer developer questions with fewer tokens, comparable or better accuracy, and access to information that text search fundamentally cannot produce (transitive call chains, test coverage gaps, cyclomatic complexity rankings). We evaluate this hypothesis using RNA (Repo-Native Alignment), an early-stage local context discovery tool, benchmarked against standard Claude Code tooling (Grep/Read/Glob) on realistic developer questions against a 25K-line Rust codebase. Results from N=5 paired runs show [TOKEN_REDUCTION]% fewer tokens with [QUALITY_DELTA] quality scores, with RNA's strongest advantages in multi-hop traversal and complexity analysis.

## 1. Introduction

### 1.1 The Context Assembly Problem

Large language models compress common human knowledge during training. But every codebase contains fractal, local knowledge not in training data: architecture decisions, naming conventions, dependency topology, business intent. An agent working in an unfamiliar codebase must discover this knowledge through tool use — and the efficiency of that discovery directly determines task success, cost, and latency.

Current coding agent harnesses (Claude Code, Codex, OpenCode) provide text-based tools: grep for content search, file reading for comprehension, glob for file discovery. These tools treat code as text. But code is not text — it is a graph of typed relationships: functions call functions, structs contain fields, modules import modules, tests exercise production code, business outcomes connect to implementation through commit history.

### 1.2 Two Layers of Context Assembly

Context for a coding agent comes from two sources:

**Prompt context** — what's assembled into the system prompt and conversation before the model reasons: CLAUDE.md/AGENTS.md instructions, skill definitions, MCP tool descriptions, prior conversation turns. This is curated at the harness level and determines the agent's behavior, constraints, and available capabilities. Prompt context engineering is about selecting, ordering, and sizing this information to maximize the model's instruction-following and task comprehension.

**Local context** — what's discoverable in the working environment: code, documentation, configuration, git history, build artifacts, business artifacts. This is queried during the task via tool use. Local context is orders of magnitude larger than prompt context (a codebase is millions of tokens; a system prompt is thousands) and must be accessed selectively.

These layers interact. Prompt context tells the agent *how* to explore; local context is *what* it explores. A well-engineered prompt with poor local context tools produces an agent that knows what to do but can't find what it needs. Good local context tools with a poor prompt produces an agent that finds information but doesn't know how to use it.

RNA addresses the local context layer: making the fractal, structural knowledge in a codebase queryable so the agent retrieves precisely what it needs, not everything that matches a keyword. The prompt context layer (how agents are instructed to use RNA, how tool descriptions are worded, how much system prompt budget MCP tools consume) is a separate engineering challenge that significantly affects RNA's effectiveness — as our benchmark demonstrates.

### 1.3 Codebases as Knowledge Graphs

A codebase encodes three layers of knowledge:

1. **Structural knowledge** — what exists (functions, types, modules) and how they relate (calls, imports, implements, defines). This is a directed graph recoverable through static analysis.

2. **Semantic knowledge** — what concepts are represented and how they cluster. "Authentication" spans multiple files and function names; "payment processing" may not appear as a literal string anywhere. This requires vector representations.

3. **Intentional knowledge** — why things exist, what outcomes they serve, what constraints govern them. This lives in documentation, commit messages, and (when explicitly declared) business artifacts. This requires both vector search (discovery) and graph structure (tracing intent to implementation).

Text search (grep) can access layer 1 for single-hop queries ("where is function X?") but fails for multi-hop queries ("what transitively depends on X?"). It cannot access layer 2 at all (semantic similarity requires embeddings). It can access layer 3 only through keyword matching, which is fragile.

### 1.3 Hypothesis

A hybrid vector-graph representation that pre-computes structural relationships (via static analysis and LSP) and semantic embeddings (via transformer models) will enable coding agents to:

- **Use fewer tokens** to answer the same questions (fewer tool calls, less irrelevant context)
- **Achieve equal or better answer quality** (more complete, more specific, fewer factual errors)
- **Access information text search cannot produce** (transitive dependencies, complexity metrics, test coverage gaps)

### 1.4 Generalization Beyond Code

This hypothesis generalizes beyond codebases. Any corpus of knowledge work artifacts — design documents, process specifications, organizational structures — can be represented as a graph of concepts with typed relationships. The same hybrid approach (embedding for discovery, graph for traversal) should apply to:

- Legal document corpora (statutes reference statutes, cases cite cases)
- Engineering specifications (requirements trace to tests, components connect to subsystems)
- Business process models (activities depend on activities, roles connect to responsibilities)

We focus on codebases because they provide the tightest feedback loop: tool calls are logged, answers are verifiable against source code, and the ground truth (what the code actually does) is available for evaluation.

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

unified-hifi-control: a 25K-line Rust application with 5 adapters (Roon, LMS, OpenHome, UPnP, HQPlayer), an event bus architecture, web UI (Dioxus), and MCP server. The codebase was unknown to the benchmark designers during question formulation.

Graph statistics after RNA indexing:
- 15,132 nodes (code symbols + markdown sections)
- 23,576 tree-sitter edges + 5,067 LSP call edges = 28,643 total
- Embedding index: ~5,000 vectors (embeddable symbols)

### 3.3 Agents

**Vanilla agent**: Standard Claude Code tooling — Grep, Read, Glob, Bash. No MCP servers. Instructed to use only these tools.

**RNA agent**: Same base tools + RNA CLI accessible via Bash. Prompt includes exact RNA CLI commands for each question (e.g., `search --mode impact --hops 3`). RNA index pre-built with warm cache.

Both agents use the same model (Claude Opus 4.6, 1M context), same token budget, same question prompt (minus tool-specific instructions).

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

**Harness limitations**: RNA is accessed via CLI (Bash commands) rather than native MCP integration, adding per-call overhead (~150ms cache load). Native MCP integration was not testable in this benchmark configuration because the MCP server is bound to the working directory.

**Single codebase**: Results are from one Rust project. Generalization to other languages, project sizes, and architectures is not established.

**Single model**: All runs use Claude Opus 4.6. Results may differ with other models or model versions.

**Scorer bias**: The scorer is also an LLM, subject to its own biases. We mitigate by scoring independently (not comparatively) and using anchored rubric examples.

**Small sample**: N=5 per condition. Statistical significance is limited. Results should be interpreted as directional evidence, not definitive proof.

**Developer of RNA conducted the benchmark**: The benchmark was designed and run by the RNA developer. Independent replication would strengthen the findings.

## 4. Results

### 4.1 Efficiency Metrics

| Metric | Vanilla (mean ± sd) | RNA (mean ± sd) | Delta |
|---|---|---|---|
| Total tokens | [TODO] | [TODO] | [TODO]% |
| Wall time (s) | [TODO] | [TODO] | [TODO]% |
| Tool calls | [TODO] | [TODO] | [TODO] |

### 4.2 Quality Scores

| Criterion (weight) | Vanilla | RNA | Delta |
|---|---|---|---|
| Q1 Volume flow (25%) | [TODO] | [TODO] | [TODO] |
| Q2 Adapter checklist (20%) | [TODO] | [TODO] | [TODO] |
| Q3 Test coverage (20%) | [TODO] | [TODO] | [TODO] |
| Q4 Zone blast radius (20%) | [TODO] | [TODO] | [TODO] |
| Q5 Complexity ranking (15%) | [TODO] | [TODO] | [TODO] |
| **Weighted total** | [TODO] | [TODO] | [TODO] |

### 4.3 Qualitative Observations

[TODO: Fill from actual results]

- Q5 (complexity): RNA provides cyclomatic complexity numbers that grep cannot compute
- Q1 (volume flow): Both agents trace similar chains; RNA may find more transitive connections
- Q3 (test coverage): RNA's tests_for mode systematically checks each function; grep approximates via filename matching

## 5. Discussion

### 5.1 Where Graph Augmentation Helps

[TODO]

### 5.2 Where Text Search Is Sufficient

[TODO]

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

- **12-Factor Agents / Context Engineering** (humanlayer.dev) — Framework for systematic context management. Distinguishes harness engineering (tool configuration) from context engineering (what information reaches the model). RNA implements the "give agents the right context" principle via queryable pre-indexed artifacts.
- **Harness Engineering** (Viv Trivedy, vtrivedy.com) — Identifies four customization levers: system prompt, tools/MCPs, context, sub-agents. Our benchmark measures the tools/MCPs lever specifically — does a better code exploration tool improve outcomes?

## 7. Conclusion

[TODO: Fill after results]

## Appendix A: Benchmark Prompts

### A.1 Vanilla Prompt
[Include full prompt-v3.md]

### A.2 RNA Prompt
[Include full prompt-v3-rna.md]

### A.3 Evaluation Rubric
[Include full eval-rubric-v3.md]

## Appendix B: Raw Results

[TODO: Include per-run token counts, scores, and solution texts]
