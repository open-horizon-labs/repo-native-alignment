# Repo-Native Alignment

[![CI](https://github.com/open-horizon-labs/repo-native-alignment/actions/workflows/rust-main-merge.yml/badge.svg)](https://github.com/open-horizon-labs/repo-native-alignment/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

Coding agents are blind to the shape of your codebase. RNA fixes that: it runs as an MCP server alongside your agent and gives it one call to answer "what depends on this?", "what relates to payment processing?", or "how is the reliability outcome progressing?" — questions LSP alone cannot answer.

Single binary. No Docker. No external database. No API key.

**[Quick Start](#quick-start)** | **[Why RNA](#why-rna)** | **[MCP Tools](#mcp-tools)** | **[Docs](#detailed-documentation)**

---

## Why RNA

LSP gives agents single-symbol, single-hop, single-language queries. There's no multi-hop primitive, no semantic search, no connection to business artifacts.

**Finding blast radius with LSP alone:**
```
textDocument/references("ConnectionPool")     → [PoolManager, HttpServer, Worker]
callHierarchy/incomingCalls(PoolManager)      → [AppConfig, TestHarness]
callHierarchy/incomingCalls(HttpServer)       → [main, Router]
callHierarchy/incomingCalls(Worker)           → [Scheduler]
// agent must: filter test files, deduplicate, reason about the shape
```

**With RNA:**
```
search(query="ConnectionPool", mode="impact", max_hops=3)
→ PoolManager → AppConfig
→ HttpServer → main, Router
→ Worker → Scheduler
// production code ranked first, test files demoted, cross-language
```

| Job | LSP alone | RNA |
|---|---|---|
| What breaks if I change this? | N round-trips of `incomingCalls`, agent assembles graph | `search(mode="impact")` — one call, transitive |
| Find code related to a concept | No semantic search — agent must guess names and grep | `search("payment processing")` — ranked by meaning across code, docs, and artifacts |
| How is our reliability outcome progressing? | Not possible — LSP has no business context | `outcome_progress("reliability")` — commits → files → symbols |
| Orient a new agent to the repo | Multiple searches, no subsystem picture | `repo_map()` — subsystems, hotspots, entry points in one call |

RNA runs LSP servers internally. It fuses their data with tree-sitter, embedded function bodies, git history, and business artifacts into a cross-language graph — so the agent gets the LSP depth without doing the LSP orchestration.

For Rust, exact search and graph traversal also expose struct-literal construction sites. A struct query returns both its declaration and local initializers, while `impact` traces the explicit `Constructs` relationships without requiring LSP or embeddings.

## What Changes After Installing

Six jobs agents can do after RNA is running that they could not do reliably before:

**Find code by meaning, not just by name**
`search("payment processing")` returns ranked results across symbols, docs, commits, and artifacts in one call. Path scoping works too: `search("auth/handlers/validate")` returns only symbols named `validate` in files matching `auth/handlers`.

When a compiler or test runner already provides a location, `search(file="src/service.rs", line=120, end_line=130)` returns that exact, numbered span from the current filesystem. Compiler-style locations such as `search(file="src/service.rs:120:7")` work too. Source spans are root-scoped, reject ambiguous or escaping paths, and are capped at 200 lines and 64 KiB of source text per request.

**Trace call paths and blast radius**
`search(node="AuthHandler", mode="impact")` returns transitive dependents grouped by subsystem. `search(node="X", mode="path", query="Y")` returns the directed call chain between two nodes.

**Know which capabilities are ready**
`scan`, `enrich`, and `list_roots` surface OperationReport summaries: ready capabilities, degraded query classes, recent enrichment jobs, and the exact follow-up command when embeddings or call/reference enrichment is missing.

For LSP enrichment, `list_roots` also reports query yield by language/server, operation, and declaration class: scheduled requests, non-empty responses, emitted edges, latency, timeouts, and errors. This makes the cost and agent-visible value of each compiler query profile inspectable.

**Connect code to business outcomes**
`outcome_progress("agent-alignment")` follows tagged commits to changed files to affected symbols. Outcomes, signals, and guardrails in `.oh/` are full graph nodes — searchable, linkable, tracked.

**Orient instantly in an unfamiliar repo**
`repo_map()` returns detected subsystems (from actual call relationships), top symbols by PageRank, hotspot files, and active outcomes. One call instead of an exploratory loop.

**Inspect schemas and service contracts**
Built-in extractors cover SQL, OpenAPI/JSON Schema, and `.proto` files, so agents can find database objects, API operations, protobuf messages/services/RPCs, and gRPC call links in the same graph as application code.

## Quick Start

### 1. Install

**Claude Code users** (recommended):

```bash
# 1. Add the marketplace
claude plugin marketplace add open-horizon-labs/repo-native-alignment

# 2. Install the plugin
claude plugin install rna-mcp

# 3. Restart Claude Code, then run the setup skill:
/rna-mcp:setup
```

Setup detects your platform (optimized binary for M2+ chips with bf16/i8mm CPU instructions), installs a release artifact to `~/.cargo/bin/`, configures `.mcp.json`, and updates AGENTS.md with tool guidance.

**Download a prebuilt binary** (manual):

```bash
# macOS Apple Silicon (M2+ optimized — bf16/i8mm)
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-darwin-arm64-fast.tar.gz | tar xz -C ~/.cargo/bin

# macOS Apple Silicon (M1 baseline)
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-darwin-arm64.tar.gz | tar xz -C ~/.cargo/bin

# Linux x86_64
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-linux-x86_64.tar.gz | tar xz -C ~/.cargo/bin
```

Release and user-facing verification should use successful GitHub Actions/release artifacts, not a local source build.

Prebuilt release binaries are intentionally built without local embedding/reranking support. They support extraction, graph traversal, lexical search, LSP call/reference enrichment, repo maps, and MCP delivery. Semantic search and cross-encoder reranking require a development/source build with embedding features (for Apple Silicon Metal: `cargo install --locked --path . --features metal` from a checked-out repo). Do not use source builds as release verification; release verification must install the successful GitHub Actions/release artifact for the target commit.

**Build from source for development** (requires [Rust toolchain](https://rustup.rs)):

```bash
git clone https://github.com/open-horizon-labs/repo-native-alignment.git
cd repo-native-alignment
cargo install --locked --path .
```

### Reproduce the RustSec release decision

Rust CI audits the authoritative `Cargo.lock` with a pinned scanner and rejects
vulnerabilities, undeclared warnings, stale policy records, and expired warning
decisions. Run the same decision locally:

```bash
cargo install cargo-audit --version 0.22.2 --locked
python3 .github/scripts/check-rustsec-policy.py --self-test
python3 .github/scripts/check-rustsec-policy.py --live
```

Warning decisions and their dependency paths, exact feature reachability,
upstream status, impact rationale, owners, removal issues, review triggers, and
expiries live in `security/rustsec-policy.json`. Live mode recomputes direct
dependents for the default, embeddings, and Metal graphs and fails when they
drift. The policy does not use cargo-audit advisory ignores.

### 2. Connect to your MCP client

The MCP server command is `repo-native-alignment` with `--repo` as an argument. Use a direct binary path for `command` — the `--repo` flag goes in `args`, not in `command`, and MCP stdio launchers should not rely on shell splitting or shell-profile/tool-manager wrappers.

Example `.mcp.json`:

```json
{
  "mcpServers": {
    "rna-server": {
      "type": "stdio",
      "command": "/Users/me/.cargo/bin/repo-native-alignment",
      "args": ["--repo", "/path/to/your/project"]
    }
  }
}
```

Keep `command` as the direct RNA binary. Do not launch RNA through `mise`,
`asdf`, `brew`, another tool-manager command, or a wrapper script.

#### Optional C#/.NET LSP enrichment

Only investigate or configure .NET when the repository contains C#/.NET markers
(such as `.cs`, `.csproj`, or `.sln` files), `repo-native-alignment setup`
reports that the optional C# toolchain is needed, or the user explicitly asks
for C# enrichment. Other repositories need no .NET environment configuration.

C# call/reference enrichment requires `dotnet` and `csharp-ls`. Install
`csharp-ls` with `dotnet tool install -g csharp-ls` and ensure
`~/.dotnet/tools` is on `PATH`. If the MCP launcher does not inherit the shell
environment, add the appropriate `DOTNET_ROOT` (and `DOTNET_ROOT_ARM64` on
Apple Silicon) plus the .NET root and `~/.dotnet/tools` to the server's `env`
block. Homebrew, mise, asdf, and official installs use different roots; use the
root reported by the setup command. The MCP `command` must still be the direct
RNA binary, never `mise`, `asdf`, `brew`, or another wrapper.

For HTTP transport: `repo-native-alignment --repo . --transport http --port 8382`

### 3. Build the index

```bash
repo-native-alignment scan --repo . --full
```

Runs the complete release-binary pipeline: scan → extract → LSP enrich → graph. Without `--full`, LSP analysis is skipped — subsystem detection and "what calls this" queries are degraded until call/reference enrichment completes. Release binaries do not include embedding/reranking support; builds compiled with `--features embeddings` or `--features metal` also run embedding enrichment. Subsequent scans are incremental (~0.1s on no-change runs).
OperationReport output tells you which capabilities are ready, which query classes are degraded, and which `enrich` command can close any remaining gap.

**Why run this before starting the MCP server?** The MCP server pre-warms the graph automatically at startup, but building from scratch can take 10-30s on large repos. Running `scan` first populates `.oh/.cache/lance/` so the server loads the cached graph in seconds.

### 4. Verify

```bash
repo-native-alignment test --repo /path/to/your/project
```

Runs 29 checks end-to-end. Exits 0 on pass, 1 on failure. Safe to run in CI.

### 5. Try it from the CLI

Before wiring up MCP, evaluate RNA directly:

```bash
repo-native-alignment search "auth" --repo /path/to/your/project
repo-native-alignment graph --node "<stable-id-from-search>" --mode impact --repo .
```

### 6. Teach your agents (optional — requires [OH Skills](https://github.com/open-horizon-labs/skills))

Install OH Skills ([see instructions](https://github.com/open-horizon-labs/skills#install)), then open a Claude Code session in your project and run:

```
/teach-oh
```

This explores your codebase, asks about your aims, writes `AGENTS.md`, scaffolds `.oh/` with outcomes and constraints, and installs phase agents.

## MCP Tools

| Tool | What it's for |
|------|--------------|
| `search` | Code symbols, artifacts, commits, and markdown — flat or graph traversal (`mode`: neighbors, impact, reachable, tests_for, cycles, path). Retrieve bounded current-filesystem source with `file` + `line`/`end_line` (or `file="path:line:column"`; maximum 200 lines). Scope to a subsystem (`subsystem=`), filter cross-subsystem edges (`target_subsystem=`), use `compact: true` for ~25x fewer tokens, `rerank: true` (default for MCP) for cross-encoder precision. Use `include_body: true` (requires `node` or `nodes`) to return function bodies; add `minify_body: true` to strip comments and shorten locals with a legend (tree-sitter AST for TS/JS, Rust, Python, Go; text fallback for others). |
| `repo_map` | Repository orientation: detected subsystems with their key interfaces, top symbols by importance, hotspot files, active outcomes, entry points. One call replaces an exploratory loop. |
| `outcome_progress` | Connect business outcomes to code: outcome → tagged commits → changed files → symbols. Optional `include_impact: true` for risk-classified blast radius. |
| `list_roots` | Show configured workspace roots with live scan stats (symbols, edges, detected frameworks, LSP edge counts per language, scan phase), durable Pass 1 queue snapshots (pending, in-flight, completed, failed, skipped, exhausted, resumed/retried counts, phase counts, oldest work, and bounded actionable exhaustion samples) from `.oh/.cache/lsp_pass1_work_items.json`, and recent OperationReport history from `.oh/.cache/operation_reports.json`. Interrupted queues resume only eligible work: completed output and skipped state are carried forward only when the node input fingerprint is unchanged. OperationReports persist the same queue snapshot for post-run inspection. Includes LSP servers available to install for each root's detected languages. |

Markdown is source-addressable beyond headings: body nodes for paragraphs, blockquotes,
links, footnotes, tables, images, code fences, and HTML directives carry stable structural
or explicit IDs, exact line/byte spans, and content hashes. Exact same-file and
repository-relative cross-file anchors such as `target.md#anchor` become graph
references; unresolved anchors are exposed as diagnostics.

Content-derived custom edges can retain ordered body selectors, line references,
snippets, hashes, pack/rule identity, confidence, and validation state through
persistence and MCP traversal. Evidence is revalidated against current Markdown
body nodes on load; edited or missing evidence is downgraded from confirmed.
Frontmatter-only relationships remain detected candidates until a body-backed
rule supplies valid evidence.

**Root scoping:** All query tools default to the primary workspace root (`--repo`). Pass `root: "all"` for cross-root search, or `root: "<slug>"` for a specific root.

**Worktree-aware queries:** Agents working in a git worktree can query their own code by passing the absolute path: `search(query="...", repo="/absolute/path/to/worktree")`. The worktree must be scanned first.

**Index-updating indicator:** When the background scanner is actively rebuilding the index (triggered by a HEAD change), `search`, `repo_map`, and `outcome_progress` responses append: `_Index updating in background — results reflect last complete scan._` No note appears when the index is current. This is informational — results are still valid, just from the previous complete scan.

**Search degradation:** If semantic scoring fails after a graph is mapped, `search` keeps the graph available, returns bounded lexical/graph results, and appends content-safe component/model/index diagnostics. Rebuild the embeddings capability for that root and retry semantic search; the diagnostic deliberately omits query text, file paths, and panic payloads.

### CLI ↔ MCP Equivalence

CLI and MCP share the same index. Run `scan --full` from the CLI to build the complete index, then query via either interface.

| CLI | MCP | What it does |
|-----|-----|-------------|
| `search "auth"` | `search(query="auth")` | Find symbols by name |
| `graph --node <id> --mode neighbors` | `search(node="<id>", mode="neighbors")` | Graph traversal |
| `scan --full` | *(runs automatically on first query)* | Full pipeline: scan → extract → embed → LSP → graph |
| `enrich --capability embeddings\|call-references --scope repo` | — | Run one enrichment capability against an existing cache without re-extracting source; use OperationReport output to see readiness/degradation. |
| `test` | — | 29 pipeline checks end-to-end |

### CLI Subcommands

| Command | What it does |
|---------|-------------|
| `search <query>` | Search symbols by name, keyword, or meaning — filter by kind/language/file |
| `graph --node <id> --mode <mode>` | Traverse neighbors, impact analysis, or reachability |
| `scan --repo <dir>` | Scan + extract + embed + persist. Prints an OperationReport summary with ready capabilities, degraded query classes, and next enrichment commands. |
| `--business-context enabled\|disabled <command>` | Global context mode. `disabled` excludes `.oh/**`, automatic business-context preambles, and Git-history producers while preserving ordinary repository files; incompatible disposable caches are deleted and rebuilt before use. Defaults to `enabled`. |
| `scan --repo <dir> --timings` | Include measured operation phase timings in the scan summary. Unmeasured subphases are omitted rather than inferred. |
| `scan --repo <dir> --full` | Full pipeline including LSP enrichment. Incremental on repeat runs. Persists a recent OperationReport for `list_roots`. |
| `enrich --repo <dir> --capability embeddings\|call-references --scope repo\|root\|changed\|targets\|task` | Run selected enrichment against an existing graph cache without source extraction. Broad type/constant references are default-off and run only for an explicit `changed`, `targets`, or `task` scope with visible `--max-requests` and `--max-duration-ms` limits. Use repeated `--target-symbol` values for `targets`; use `--task-file` and/or `--target-symbol` for `task`. Scoped broad-reference work never continues repo-wide, persists `default_profile`/full/scoped/partial/unavailable evidence, and rejects empty, ambiguous, or unmapped declarations rather than widening the request. |
| `stats --repo <dir>` | Show repo stats from persisted index (no re-scan) |
| `outcome-progress <id> --repo <dir>` | Join outcome artifacts, tagged commits, and impacted symbols |
| `list-roots` | Show configured roots, scan state, readiness, and recent OperationReports |
| `repo-map --repo <dir>` | Show subsystems, top symbols, hotspots, and active outcomes |
| `test --repo <dir>` | Run 29 pipeline checks end-to-end |
| `adr compile --repo <dir>` | Compile ADR frontmatter into `.oh/adr-validation/*.json` and refresh `docs/ADRs/README.md` when present |
| `adr validate --repo <dir>` | Execute compiled ADR validations (cargo tests, audits, smoke, scripts) and fail if an implemented ADR is not honestly backed by passing checks |
| `adr audit <name> --repo <dir>` | Run one built-in ADR code-shape audit directly |
| `open --repo <dir>` | Launch an interactive graph visualizer in the browser |
| `setup --project <dir>` | Bootstrap RNA + OH MCP + skills for a project |


Recent scan/enrich operation history is bounded diagnostic state under `.oh/.cache/operation_reports.json`. It is repo-native control-plane state for CLI/MCP visibility, not source truth; non-terminal records from a previous process are marked stale on read.

Use `--business-context disabled` only when a run must exclude RNA-specific business and history context, such as a benchmark. The mode is producer-based: README, Markdown, RST, source, tests, and configuration paths are not excluded by the context policy. Extraction and LSP availability for each admitted file type still follow the repository's configured baseline capabilities.

### ADR validation primitive

ADRs can declare direct executable references in YAML frontmatter:
- `validate.cargo_tests` — exact names from `cargo test -- --list`
- `validate.audits` — built-in code-shape audits run with `adr audit <name>`
- `validate.smoke` — fixture paths for repo-native delivery checks
- `validate.scripts` — exact repo-relative script paths as a last resort

Compile declarations into derived manifests and validate them with the real executable surface:

```bash
repo-native-alignment adr compile --repo .
repo-native-alignment adr validate --repo . --cargo-arg --no-default-features
```
### Plugin Skills

| Skill | What it does |
|-------|-------------|
| `/rna-mcp:setup` | Download binary, configure MCP, update AGENTS.md |
| `/rna-mcp:dead-code` | Find public functions with zero non-test callers using RNA's graph (no code changes). Heuristic — false positives exist for framework callbacks, trait impls, and FFI exports. Accuracy depends on a `scan --full` having completed with LSP enrichment; without LSP-derived `Calls`/`ReferencedBy` edges every function looks dead. See [`plugin/skills/dead-code/SKILL.md`](plugin/skills/dead-code/SKILL.md). |
| `/rna-mcp:review-readiness` | Agent-led PR/working-tree review triage. Starts from the diff, adds RNA graph/business context only where it changes review decisions, and states when raw diff review is enough. See [`plugin/skills/review-readiness/SKILL.md`](plugin/skills/review-readiness/SKILL.md). |
| `/rna-mcp:record` | Record business artifacts (metis, signals, guardrails, outcome updates) with frontmatter templates |

## The `.oh/` Directory

```
.oh/
├── outcomes/        <- what we're optimizing for
├── signals/         <- how we measure progress
├── guardrails/      <- constraints that shape behavior
├── metis/           <- learnings that compound across sessions
├── config.toml      <- scanner excludes, LSP severity threshold, declared workspace roots
├── extractors/      <- custom boundary detection configs (optional, see below)
└── .cache/          <- scan state, embedding index (gitignored)
```

Business artifacts (`outcomes/`, `signals/`, `guardrails/`, `metis/`) are committed to git. `.cache/` is gitignored and rebuilt automatically on first query.

### Workspace roots

Declare intentionally related repos in `.oh/config.toml`:

```toml
[scanner]
exclude = ["benchmark/"]

[lsp]
# Minimum severity to store as diagnostic nodes.
# "error" | "warning" (default) | "information" | "hint"
diagnostic_min_severity = "hint"

[workspace.roots]
infra   = "../k8s-configs"
protos  = "/abs/path/protos"
```

After declaring roots, run `scan` (or restart RNA). Roots appear in `list_roots()` and are queryable by slug:

```text
search(root="infra", query="Deployment")  # only K8s results
search(root="all")                        # all roots
```

### Custom boundary detection

Declare custom pub/sub or event-bus boundary patterns in `.oh/extractors/*.toml`. RNA reads these at scan time and emits `Produces`/`Consumes` edges without any changes to RNA source.

```toml
# .oh/extractors/internal-event-bus.toml
[meta]
name = "internal-event-bus"
applies_when = { language = "python", imports_contain = "src.events.bus" }

[[boundaries]]
function_pattern = "bus.publish"
arg_position = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "bus.subscribe"
arg_position = 0
edge_kind = "Consumes"
```

Fields: `applies_when.language`, `applies_when.imports_contain`, `function_pattern` (substring or glob), `arg_position` (zero-indexed string literal argument holding the topic), `edge_kind` (`"Produces"` or `"Consumes"`), `decorator` (set `true` when matching a decorator name).

RNA includes built-in extractors for kafka-python, kafkajs, celery, pika, and redis-py. Use `.oh/extractors/` for any other broker or custom RPC framework.

RNA also indexes agent rule/memory files automatically:

| Path pattern | `artifact_types` filter |
|---|---|
| `.cursorrules`, `.cursor/**` | `cursor-rule` |
| `.clinerules` (file) | `cline-rule` |
| `.serena/memories/**` | `serena-memory` |
| `.github/copilot-instructions.md` | `copilot-instruction` |

## Compared To

See the [full comparison](docs/compared-to.md) for details.

| | **RNA** | **Code-Graph-RAG** | **CodeGraphContext** | **Serena** |
|---|---|---|---|---|
| **Install** | Single binary | Docker + Memgraph + API key | pip + graph DB | `pip install mcp-server-serena` |
| **External deps** | None | Docker, Memgraph, LLM API | Graph DB (KuzuDB/Neo4j) | None (language servers auto-downloaded) |
| **Language coverage** | Broad tree-sitter extraction plus LSP enrichment | Tree-sitter only | Tree-sitter only | LSP-backed |
| **Embeddings** | MiniLM-L6-v2 on Metal GPU | UniXcoder | None | None |
| **Business context** | Outcomes, signals, guardrails, metis | None | None | Agent memories (auto-accumulated, not curated outcomes) |

## Optional: Companion Systems

RNA works standalone. These add organizational context and workflow structure:

- **[OH MCP](https://github.com/cloud-atlas-ai/oh-mcp-server)** — cross-project context: missions, aims, endeavors, decision logs
- **[OH Skills](https://github.com/open-horizon-labs/skills)** — workflow skills: `/aim`, `/review`, `/dissent`, `/salvage`, `/solution-space`, `/execute`

## Status

RNA ships a compact MCP surface, CLI parity through the shared service layer, broad tree-sitter extraction, auto-detected LSP enrichment, event-driven indexing, and automatic subsystem/framework detection.

Current release-line capabilities include durable enrichment jobs, OperationReport readiness telemetry, explicit `enrich` control, ADR validation commands, proto/schema extraction, dead-code detection, and diff-scoped review-readiness skills.

### Platform Support

| Platform | Status | Embeddings |
|----------|--------|------------|
| macOS Apple Silicon (ARM) | Full support | Metal GPU (fast) |
| Linux x86_64 | Supported | CPU-only (slower semantic search) |
| Windows | Untested | — |

## License

MIT — see [LICENSE](LICENSE).

## Detailed Documentation

- [RNA Familiar Archetype](docs/rna-familiar-archetype.md) — hosted GitHub App product/runtime archetype: familiar as app, RNA as repo cognition layer
- [Compared To](docs/compared-to.md) — RNA vs Code-Graph-RAG, CodeGraphContext
- [Extractors](docs/extractors.md) — tree-sitter language/schema extractors, constants, synthetic literals, SQL/OpenAPI/proto support
- [LSP Enrichment](docs/lsp-enrichment.md) — auto-detected language servers
- [Scanner](docs/scanner.md) — incremental, event-driven, worktree-aware scanning, dirty-slugs optimization
- [Graph Architecture](docs/graph.md) — edge types, persistence, in-memory index
- [One-instance SWE-bench RNA harness](docs/swebench-rna-one.md) — isolated Verified task, real MCP executor, official evaluator, and auditable token/cost ledger
- [ADR Index](docs/ADRs/README.md) — architecture decisions and executable validation references
- [Source Compatibility](docs/rna-source-compatibility.md) — source-capability design for future Context Assembler integration
