# Benchmark v2 Design: Graph-Augmented Context for Coding Agents

## Why re-benchmark

v1 (published with v0.1.9) tested on a single 25K-line Rust codebase with N=5 and a single model. Since then:

- **Subsystem detection** (v0.1.11) — Louvain community detection enables an entirely new category of architectural questions
- **Subsystem-scoped search** — `search(subsystem="server")` reduces noise
- **Cross-subsystem queries** — `search(target_subsystem="server")` shows boundary coupling
- **Incremental --full** — second scan in 0.1s vs 50s
- **BLAKE3 hash skip** — re-embed only changed symbols
- **Cross-encoder reranking** — improves NL query precision (reduces prompt asymmetry)
- **Short ID resolution** — seamless graph traversal from search results

The field has also moved: ContextBench, FeatureBench, Multi-SWE-bench, and the harness engineering research all provide new evaluation frameworks.

## Strategic framing

### The harness problem

Same model + different harness = wildly different results. Can Boluk showed a 10x improvement (6.7% → 68.3%) by changing only the edit format. LangChain showed 13.7 points from harness engineering alone. ETH Zurich showed LLM-generated AGENTS.md *hurts* performance.

**RNA is a harness component.** Its value proposition is not "better model" but "better local context assembly." The benchmark should measure the harness effect, not the model.

### What the research validates

| Finding | Source | RNA implication |
|---------|--------|-----------------|
| Graph+embed beats embed-only by 43% | CodeRAG (arXiv 2504.10046) | RNA's hybrid approach is validated |
| Graph-integrated LLM: +12.33% SWE-bench Lite | Code Graph Model (arXiv 2505.16901) | Structural code understanding works |
| Active exploration beats passive dumps (F1: 0.676 vs 0.457) | Theory of Code Space (arXiv 2603.00601) | Query-based MCP > static context injection |
| Context rot at every length increment | Chroma (2025) | Less context, more precise = better |
| 97.1% of MCP tool descriptions have quality defects | arXiv 2602.14878 | Tool description quality matters |
| "Bitter lesson" — sophisticated scaffolding yields marginal gains in context retrieval | ContextBench (arXiv 2602.05892) | RNA must show *precision* gains, not just recall |
| FeatureBench: best models solve only 11-12.5% | ICLR 2026 | Massive headroom for structural understanding |

### The ContextBench challenge

ContextBench's "bitter lesson" finding is the strongest counter-argument to RNA: sophisticated retrieval scaffolding may yield only marginal context retrieval gains. RNA must demonstrate:

1. **Precision over recall** — returning precisely the relevant subgraph, not everything that matches
2. **Multi-hop efficiency** — collapsing sequential tool calls into single queries
3. **Information text search cannot produce** — cyclomatic complexity, transitive dependencies, test coverage mapping

## Benchmark design

### Target benchmarks (external)

| Benchmark | What it measures | Why RNA should win | Risk |
|-----------|-----------------|-------------------|------|
| **FeatureBench** | Feature development (not just bug fixes), 11% solve rate | Feature dev requires architectural understanding — graph + subsystems | RNA's cold-start cost may negate gains on short tasks |
| **ContextBench** | Context retrieval precision/recall | Graph queries return structural context that keyword search misses | ContextBench's bitter lesson — marginal gains possible |
| **RepoBench-R** | Cross-file retrieval accuracy | RNA's dependency edges identify cross-file relationships | Embed-only baselines may be competitive on simple lookups |
| **DependEval** | Dependency understanding (construction, recognition, multi-file edit) | RNA literally builds the dependency graph | May be "too easy" — RNA has the answer directly |
| **Multi-SWE-bench** | Multi-language repo-level issue resolution | Tree-sitter + LSP across languages | LSP quality varies by language |

### RNA-specific benchmark (custom, v2)

#### Codebases (minimum 3)

| Codebase | Size | Language(s) | Why |
|----------|------|-------------|-----|
| unified-hifi-control | 25K lines | Rust | Continuity with v1 |
| FastAPI/SQLAlchemy web app (TBD) | 50-100K lines | Python | Dynamic language, larger scale, different LSP |
| TypeScript monorepo (TBD) | 100K+ lines | TypeScript | Monorepo structure, tsserver, module resolution |
| **Stretch:** polyglot repo | varies | Go + TypeScript | Cross-language gap testing |

#### Questions (15, across 5 categories)

**A. Symbol-level (retained from v1)**
1. Multi-hop call chain tracing
2. Pattern discovery / checklist generation
3. Test coverage analysis

**B. Subsystem-level (new — tests v0.1.11)**
4. "Which subsystem handles [feature X]?"
5. "What are the architectural boundaries in this codebase?"
6. "Which subsystem has the highest/lowest cohesion?"

**C. Cross-subsystem impact (new — tests cross-subsystem queries)**
7. "If I change [core type], which subsystems are affected and through what entry points?"
8. "What connects the [API layer] to the [storage layer]?"
9. "I'm adding a feature spanning [subsystem A] and [subsystem B]. What integration points exist?"

**D. Token efficiency (new — measures scoping benefit)**
10. Repeat Q1 with `subsystem` scoping vs unscoped — measure token delta
11. Repeat Q7 with subsystem-aware impact vs full impact — measure precision

**E. Cold-start and scale (new)**
12. First question on a never-indexed repo — total time including scan
13. Same question on 100K-line repo vs 25K-line — scaling behavior

#### Conditions (4-way)

| Condition | Tools | Context state | What it measures |
|-----------|-------|---------------|-----------------|
| Vanilla | Grep, Read, Glob, Bash | N/A | Baseline |
| RNA (warm, unscoped) | RNA MCP, no subsystem filters | Pre-indexed | v1 equivalent |
| RNA (warm, scoped) | RNA MCP with subsystem filters | Pre-indexed | v0.1.11 value |
| RNA (cold) | RNA MCP | Not indexed | Real-world first-use |

#### Prompt fairness

v1 had a prompt asymmetry: RNA agent got explicit CLI commands, vanilla got no equivalent guidance.

For v2, run both variants:
- **Guided:** Both conditions get equivalent strategy hints ("use grep to trace callers" vs "use search with mode=neighbors")
- **Unguided:** No strategy hints for either — tests tool discoverability

The delta between guided and unguided directly quantifies the tool adoption problem (paper section 5.3).

#### Sample size

N=15 per condition minimum. At N=15, a 0.5-point quality difference on a 5-point scale is detectable at p<0.05 (two-sample t-test, SD≈0.5).

#### Models

| Model | Why |
|-------|-----|
| Claude Opus 4.6 | Continuity with v1, current frontier |
| Claude Sonnet 4.6 | Cost-performance trade-off, tests whether smaller models benefit more from RNA |
| GPT-5.3-Codex or GPT-5.4 | Cross-vendor validation |
| Qwen3-Coder-Next or DeepSeek V3.2 | Open-source validation |

#### Metrics

**Efficiency:**
| Metric | What it measures |
|--------|-----------------|
| Total cost (USD) | End-to-end cost |
| Wall time (seconds) | Latency |
| Tool calls per question | Tool efficiency |
| Failed tool calls | Tool usability |
| System prompt token overhead | MCP schema cost |
| Tool result tokens per call | Result verbosity |
| Tokens per category (system / tool input / tool result / reasoning) | Where efficiency comes from |

**Quality:**
| Metric | What it measures |
|--------|-----------------|
| Score per question (1-5, anchored rubric) | Answer quality |
| Functions/files correctly named | Specificity |
| Factual errors | Accuracy |
| Subsystems correctly identified | Architectural reasoning |
| Cross-subsystem edges identified | Boundary reasoning |
| Max hop depth traced | Multi-hop capability |

**Practical:**
| Metric | What it measures |
|--------|-----------------|
| Cold-start penalty (seconds) | First-use cost |
| Time to first useful answer | Perceived latency |
| Cache hit rate (BLAKE3 skips) | Re-scan efficiency |

#### Evaluation

1. **LLM scorer** with anchored rubric (retain from v1)
2. **Human evaluation** on random 20% sample — measure inter-rater agreement
3. **Automated factual verification** — for every function/file named, verify it exists in the codebase (scriptable)
4. **Independent evaluator** — question design by someone unfamiliar with RNA's capabilities

## Relationship to external benchmarks

For **FeatureBench**, **ContextBench**, and **Multi-SWE-bench**: integrate RNA as an MCP server available to the agent harness. Measure with RNA vs without RNA on the standard task sets. This produces directly comparable results against the published leaderboards.

The custom RNA-specific benchmark (above) tests capabilities the external benchmarks don't cover: subsystem reasoning, cross-subsystem impact, token efficiency from scoping.

## Timeline and effort

| Phase | What | Effort |
|-------|------|--------|
| 1. Codebase selection | Choose Python + TypeScript repos, verify LSP works | 1 day |
| 2. Question design | 15 questions with rubric, reviewed by non-RNA developer | 2 days |
| 3. Infrastructure | Benchmark runner, scorer, aggregator (extend v1 scripts) | 1 day |
| 4. Runs | 4 conditions × 15 questions × 15 runs × 4 models = 3,600 runs | 1-2 weeks (automated, ~$2K-5K in API costs) |
| 5. Analysis | Statistical analysis, paper update | 2-3 days |
| 6. External benchmarks | FeatureBench + ContextBench integration | 1 week |

## Key references

- [ContextBench](https://arxiv.org/abs/2602.05892) — "Bitter lesson of coding agents" (precision challenge)
- [FeatureBench](https://arxiv.org/abs/2602.10975) — 11% solve rate, massive headroom (ICLR 2026)
- [The Harness Problem](https://blog.can.ac/2026/02/12/the-harness-problem/) — 22-point harness swing vs 1-point model swap
- [CodeRAG](https://arxiv.org/html/2504.10046v1) — Graph+embed beats embed-only by 43%
- [Code Graph Model](https://arxiv.org/abs/2505.16901) — +12.33% SWE-bench Lite with graph-integrated LLM
- [Theory of Code Space](https://arxiv.org/html/2603.00601v3) — Active exploration beats passive dumps
- [Chroma Context Rot](https://research.trychroma.com/context-rot) — Degradation at every context length
- [ETH AGENTS.md Study](https://arxiv.org/html/2602.11988v1) — Static context files hurt performance
- [RANGER](https://arxiv.org/abs/2509.25257) — Graph+embed retrieval for repo-level code
- [MCP Tool Description Smells](https://arxiv.org/html/2602.14878) — 97.1% of tools have quality defects
- [Multi-SWE-bench](https://arxiv.org/abs/2504.02605) — 7 languages, 1,632 instances
- [DependEval](https://aclanthology.org/2025.findings-acl.373/) — Dependency understanding benchmark
- [Advanced Context Engineering](https://github.com/humanlayer/advanced-context-engineering-for-coding-agents) — Frequent Intentional Compaction
