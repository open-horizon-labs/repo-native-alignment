---
id: rna-smoke-test-design
status: proposed
date: 2026-03-08
outcome: agent-alignment
---

# RNA Smoke Test — Solution Space & Design

## Problem Statement

RNA has 153 unit and integration tests covering individual extractors, graph operations, query logic, and the `setup` subcommand. One end-to-end gap exists: no test verifies that the **full pipeline** — scan → extract → embed → index → query — works correctly when RNA runs as a live binary serving real MCP tool calls.

The existing smoke test in `.github/scripts/mcp-smoke.mjs` uses the TypeScript MCP SDK but only checks that tools are *listed*. It does not call any tool, assert on returned content, or exercise the scan/query pipeline at all. A broken `oh_search_context` implementation, a LanceDB initialization failure, or a corrupted embedding store would all pass silently.

The guardrail `test-with-real-mcp-client` (severity: candidate) documents that raw curl/pipe tests miss protocol bugs that are fatal with real MCP clients — a 45-minute regression that would have been caught by exercising actual tool calls.

### What Is Missing

| Layer | Currently tested | Gap |
|---|---|---|
| Individual extractors (Rust, Python, TS, …) | Yes — unit tests | None |
| Graph build / index operations | Yes — unit tests | None |
| Query logic (keyword + semantic) | Yes — unit tests | None |
| `setup` subcommand | Yes — `cargo test setup::tests` in CI | None |
| Binary startup + MCP protocol negotiation | Partial — `listTools` only | Tool calls never exercised |
| Scan → extract → embed → index pipeline | No | Full pipeline untested end-to-end |
| Real tool call results (context, search, graph, record) | No | Zero assertions on call output |
| CI gate for the above | No | Silent failure risk |

---

## Solution Space

### Option A: MCP TypeScript SDK smoke test (extend existing)

Extend `.github/scripts/mcp-smoke.mjs` beyond `listTools` to actually call each tool category and assert on result content.

**How it works:**
1. Connect to the RNA binary via stdio using `@modelcontextprotocol/sdk`
2. Call `oh_get_context` — assert response contains "Outcomes" or "Business Context"
3. Call `oh_search_context` with query `"agent-alignment"` — assert ≥1 result with non-empty name
4. Call `search_symbols` with query `"main"` — assert ≥1 symbol result
5. Call `outcome_progress` for outcome `"agent-alignment"` — assert structured progress section
6. Call `graph_query` with mode `"callers"` and a known symbol — assert edges or graceful empty
7. Assert non-empty on all calls; assert non-zero exit on failure

**Pro:**
- Tests the real MCP protocol layer (transport, framing, session, error propagation)
- Satisfies the `test-with-real-mcp-client` guardrail directly
- Node.js already present in CI (`rust-main-merge.yml` already installs it)
- Builds on existing infrastructure — minimal new surface

**Con:**
- Does not test content quality deeply (still treats RNA as a black box)
- Requires Node.js in CI (already there, but a dependency nonetheless)
- Cannot easily assert on embedded/semantic results without a fixture repo

---

### Option B: `rna test` subcommand (self-contained pipeline verifier)

Add a `test` subcommand to the RNA binary. When invoked, it:
1. Scans the current repo (or a temp fixture repo)
2. Builds the graph and embeddings in-process
3. Runs each query path (`oh_search_context`, `search_symbols`, `oh_get_context`, `outcome_progress`, `graph_query`, `list_roots`)
4. Emits a structured JSON or human-readable report to stdout
5. Exits non-zero if any pipeline stage fails or returns empty where content is expected

**How it works in CI:**
```
./target/release/repo-native-alignment test --repo . --format json
```
Output:
```json
{
  "pipeline": "ok",
  "stages": [
    { "stage": "scan",    "status": "ok", "files": 42 },
    { "stage": "extract", "status": "ok", "symbols": 312 },
    { "stage": "embed",   "status": "ok", "vectors": 312 },
    { "stage": "index",   "status": "ok" },
    { "stage": "oh_get_context",    "status": "ok", "content_bytes": 1840 },
    { "stage": "oh_search_context", "status": "ok", "results": 5 },
    { "stage": "search_symbols",    "status": "ok", "results": 12 },
    { "stage": "outcome_progress",  "status": "ok", "outcomes": 2 },
    { "stage": "graph_query",       "status": "ok", "edges": 7 },
    { "stage": "list_roots",        "status": "ok", "roots": 1 }
  ]
}
```

**Pro:**
- Zero external dependencies — pure Rust, runnable with one command
- Deeply tests the pipeline internals (not just the protocol boundary)
- Machine-parseable output; CI assertion is `rna test --repo . && echo passed`
- Enables Claude Code (or any agent) to use `rna test` as a verification step
- Does not require Claude API key or network access

**Con:**
- Does not test the MCP transport/protocol layer — a broken `handle_call_tool_request` dispatch would not be caught
- Adds binary surface (a new subcommand to maintain)

---

### Option C: Scripted Claude session as test

A script starts RNA, opens a Claude Code session with a fixed prompt, captures stdout, and asserts on expected content strings.

**Pro:** Tests the complete stack including Claude ↔ RNA interaction and real LLM tool-call flow.

**Con:**
- Requires Claude API key — cannot run in open CI without secrets management
- Non-deterministic (LLM responses vary)
- Slow (network round-trips per tool call)
- Not suitable as a per-commit gate

Verdict: Suitable as a manual/pre-release validation step, not a CI gate. Out of scope for the core smoke test.

---

### Option D: Rust integration test with in-process MCP client

A Rust `#[test]` that creates an `RnaHandler`, wraps it in a mock transport, and drives `handle_call_tool_request` directly.

**Pro:** Fast, no external deps, stays in Rust ecosystem, runs with `cargo test`.

**Con:**
- Does not test real binary startup or stdio transport initialization
- Writing a minimal MCP client in Rust is significant overhead (framing, JSON-RPC, session state)
- Does not satisfy the `test-with-real-mcp-client` guardrail

---

### Option E: Hybrid — Option B for every commit + Option A for pre-release (Recommended)

**CI gate (every commit / every PR):**
- `rna test --repo .` (Option B) — fast, zero deps, tests the full scan/embed/query pipeline
- Exits non-zero on any broken stage; CI fails visibly

**Pre-release / merge-to-main gate (existing job, extended):**
- Extend `.github/scripts/mcp-smoke.mjs` (Option A) to make real tool calls
- Verifies the MCP protocol layer, transport initialization, and dispatch routing
- Runs after `cargo build --release` in `rust-main-merge.yml`

**Why the hybrid wins:**
1. Unit tests already cover individual extractors and graph ops — those are not the gap
2. The gap is two independent concerns that require two different test surfaces:
   - *Pipeline correctness* (scan → embed → query returns real results) → Option B
   - *Protocol correctness* (binary speaks valid MCP, tool dispatch works) → Option A extended
3. Option B alone misses protocol regressions; Option A extended alone misses pipeline breakage before the MCP boundary
4. The hybrid uses infrastructure already in CI; Option B only requires a new subcommand

---

## Recommendation

**Implement Option E (Hybrid).**

The primary deliverable is the `rna test` subcommand (Option B) as the CI gate. The secondary deliverable is extending `mcp-smoke.mjs` to make actual tool calls (Option A extension) as the pre-release check.

Both run without a Claude API key. Neither requires additional CI dependencies beyond what is already present.

---

## What the Smoke Test Will Verify

### `rna test` (Option B — pipeline verifier)

| Stage | Assertion |
|---|---|
| Scanner initializes | No panic, scan state written |
| File walk | `files_scanned > 0` |
| Symbol extraction | `symbols_extracted > 0` |
| Embedding | `vectors_written == symbols_extracted` |
| LanceDB index | Index opens and accepts queries |
| `oh_get_context` path | Response contains "Business Context", `content_bytes > 0` |
| `oh_search_context` path | Query `"main"` returns ≥1 result |
| `search_symbols` path | Query `"main"` returns ≥1 symbol |
| `outcome_progress` path | Returns structured output, no panic |
| `graph_query` path | Returns without error (edges may be empty on minimal fixture) |
| `list_roots` path | Returns ≥1 root |
| Exit code | 0 on all-pass, non-zero on any failure |

### Extended `mcp-smoke.mjs` (Option A extension — protocol verifier)

| Check | Assertion |
|---|---|
| Binary starts and connects | `client.connect()` succeeds |
| `listTools` | Returns 8 tools including all required names |
| `oh_get_context` tool call | Response text contains "Business Context" |
| `oh_search_context("agent-alignment")` | ≥1 result with non-empty name field |
| `search_symbols("main")` | ≥1 symbol with file path |
| `outcome_progress("agent-alignment")` | Non-empty response, no RPC error |
| Error on unknown tool | `call_tool("nonexistent_tool")` returns RPC error, not a hang |
| Clean shutdown | `client.close()` completes without timeout |

---

## Acceptance Criteria

- [ ] `rna test --repo <path>` is a valid subcommand that exits 0 on a healthy repo
- [ ] `rna test` exits non-zero and prints a diagnostic when the pipeline is broken (e.g., LanceDB cannot initialize, embedder returns zero vectors, a query path panics)
- [ ] `rna test` runs in CI with zero additional dependencies (no Node.js, no API keys, no network)
- [ ] CI adds a `cargo run --release -- test --repo .` step that gates PRs
- [ ] `mcp-smoke.mjs` is extended to call at least: `oh_get_context`, `oh_search_context`, `search_symbols`, `outcome_progress`
- [ ] Each `mcp-smoke.mjs` tool call asserts on response content (not just absence of RPC error)
- [ ] `mcp-smoke.mjs` asserts that an unknown tool name returns an error rather than hanging
- [ ] Both smoke tests are runnable locally with a single command:
  - `./target/release/repo-native-alignment test --repo .`
  - `node .github/scripts/mcp-smoke.mjs ./target/release/repo-native-alignment .`
- [ ] Smoke tests do not require a Claude API key
- [ ] A broken `oh_search_context` implementation (e.g., always returns empty) causes `rna test` to fail with a clear diagnostic
- [ ] A broken MCP dispatch (e.g., wrong tool name routing) causes `mcp-smoke.mjs` to fail with a non-zero exit
- [ ] Both tests complete in under 60 seconds on a laptop-class machine

---

## Out of Scope for This Design

- Option C (scripted Claude session) — valuable for pre-release manual validation, not implemented here
- Fixture repo management (the tests use the RNA repo itself as the test fixture; a dedicated minimal fixture repo is a follow-on)
- Load/performance testing of the embedding pipeline
- Testing multi-root workspace scanning (follow-on)
