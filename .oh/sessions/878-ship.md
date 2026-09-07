---
pr: 878
issue: 877
outcome: context-assembly
branch: 877-restore-treesitter-calls
type: ship-session
---

## Ship Pipeline — PR #878
**Started:** 2026-09-07
**Branch:** 877-restore-treesitter-calls @ b8764bb · closes #877 · outcome: context-assembly

### Pre-flight
- PR is draft; CodeRabbit has not reviewed (draft gate). No review comments yet.
- RNA worktree index live (CLI); MCP tools not exposed to this agent (see .oh/friction-logs/878-ship.md).
- `grep -c 'language_name ==' src/extract/generic.rs` = 0.
- Release build started (`cargo build --release`, worktree target dir). Baseline `origin/main` release build started in scratch worktree with separate target dir (perf gate).

### Step 1: RNA-Grounded Review
**Verdict:** ADJUST
**Metis checked:** 4 (suffix-match-import-resolution, sweep-bug-class-siblings, computed-but-not-delivered, subagents-default-to-grep-not-rna)
**Guardrails checked:** 5 (no-language-conditionals-in-generic, no-linear-scan-on-graph, extract-fully-at-parse-time, no-parallel-cargo-agents, independent-final-review-for-prs)
**Findings:** 6 — fix item: Python `case_pattern` over-collects `class_pattern` class names / `keyword_pattern` keywords (verified against tree-sitter-python 0.25.0 node-types). Rest nits/measurement.
**AC3 check:** all 12 #859 Major threads have a `muness` reply (3 left open on #859 by design, reasoning given).
PR comment: https://github.com/open-horizon-labs/repo-native-alignment/pull/878#issuecomment-5573435972

### Step 2: Independent Code Review
Spawned code-reviewer with diff + AC + guardrails + metis + graph impact only (no session file). Pending.
**Verdict:** REQUEST CHANGES — https://github.com/open-horizon-labs/repo-native-alignment/pull/878#issuecomment-5573534888
Blocking #1: nested functions (Python nested def, TS/JS nested arrow/decl, class methods inside a function) stamped `scope_bindings_complete` with own-subtree bindings only -> enclosing parameter shadow invisible -> FALSE cross-file Calls edge (reproduced by reviewer with branch binary). Warnings: nested-scope test gap; Go `receive_statement.left` missing. Nits: TS `class` name site, Rust `lifetime` leaf, Python `aliased_import` original name, lexical supplement pass still tokenizes per candidate, persistence rationale undocumented.

### Step 3: Fix
- `LangConfig.binding_scope_kinds` (new; anonymous function forms). `collect_local_bindings` now walks from the outermost enclosing function-scope ancestor (Function-mapped node_kinds or binding_scope_kinds) so nested nodes inherit enclosing bindings (superset semantics). `opens_binding_scope` helper. Own-name exclusion applies to the walk root.
- Tables: Python `aliased_import` skip + `.alias` re-entry, `class_pattern` skip, `lambda` scope; TS `("class", name)` + arrow/function_expression/generator_function scopes; JS same scopes; Go `receive_statement.left` + `func_literal` scope; Rust `lifetime` skip + `closure_expression` scope; 15 uncurated configs `binding_scope_kinds: &[]`.
- search.rs: `query_terms` hoisted once per task request; lane eligibility uses `task_candidate_quality_with_query_terms`.
- generic.rs stamping site comment documents persistence rationale (internal evidence, needed for incremental scans, not rendered).
- Tests: generic.rs `nested_functions_inherit_enclosing_scope_bindings` (Python nested def + class method, TS nested decl + class method); zoo lists extended (Rust `'a` lifetime absent, Python `Point`/`thing` absent, TS class expression, Go `select` receive). import_calls.rs `nested_python_def_inherits_enclosing_parameter_shadow`, `nested_ts_and_js_functions_inherit_enclosing_parameter_shadow` (false-edge twins + positive twins, TS and JS extractors).
- Suite at b8764bb (pre-fix) with branch release binary: 158 passed / 0 failed / 0 skipped incl. Expertunities. Note: pre-flight scan of the worktree had been done by the installed 0.2.10 binary (first on PATH), which the branch binary refused ("missing source_file column"); full rescan with the branch binary fixed it.
- Commit `70247a1` pushed. `cargo test`: lib 2547/0/4 ignored, integration green. `cargo clippy --no-default-features -- -D warnings`: clean.
- Step 3 comment posted; Step 3b: `gh pr ready 878` (draft=false). Step 4 comment posted (3 new tests + 4 extended zoos; all pass).

### Step 4: Regression Oracle
Tests seeded from AC + Step 1 #1 + Step 2 #1-#6; all pass on 70247a1. Non-vacuous: reviewer reproduced the false edges pre-fix.

### Step 5-7 data gathered (posted after final gates)
- Perf gate (clean cache, `scan --full --no-lsp --no-embed --extract-only`, this worktree, interleaved x2 each): baseline main 8f4bf66 = 5.73/5.78/5.73/5.73s (mean 5.74s); branch = 5.99/5.97/5.94/5.89s (mean 5.95s) => +3.6% (< 10% gate). `symbols.lance` 19.6MB -> 19.9MB (+1.8%), edges.lance unchanged within noise. Edges on this repo: 167761 -> 167903 (+142 restored cross-file Tree-sitter Calls).
- Fixture before/after (CLI): main binary `graph --mode neighbors` on `Expertunities` and `py/caller.py:orchestrate` => "No results"; branch binary => `useQueryExpertunities` (api.ts) and `execute` (worker.py).
- Step 7b MCP (stdio, `@modelcontextprotocol/sdk` 1.30.0, server in normal mode, no TS/Python LSP servers on PATH): 8/8 checks pass — cross-file TS and Python Calls visible via `search mode=neighbors`; destructured-param shadow, nested arrow, nested decl, nested Python def emit none; `impact` on `useQueryExpertunities` lists the caller. `--cache-only` unusable with `--no-embed` caches ("requires a published semantic generation"). All negative-check node IDs verified to exist.
- Suite on post-fix 70247a1 binary: 158 passed / 0 failed / 0 skipped incl. Expertunities.
- CI: lint/test/audit pass on 70247a1; `smoke` job only runs on workflow_dispatch/tags in this repo (skipping on PRs is expected).
- CodeRabbit (after ready): 6 inline comments -> 5 fixed in `12c205d` (cache per scope, JS+TS class-field evidence, session frontmatter), 1 N/A with reasoning (test-only `.iter().find()`).
- Follow-ups filed: #879 (Java/C#/Kotlin/Ruby/PHP tables), #880 (finalizer stable_id dedup), #881 (#859 leftovers), #882 (old-schema cache hard error).

### Steps 7a / 7b / 8 / 9 / 10 (final binary 12c205d)
- Perf: baseline mean 5.83s vs branch mean 5.94s (+1.9%; 9.47s first-run cold start excluded). symbols.lance +1.8%.
- Suite: 158/0/0 incl. Expertunities. cargo test lib 2548/0/4; clippy clean; CI lint/test/audit pass; smoke job is dispatch/tag-only in this repo.
- 7b MCP stdio: 10/10 (adds class-property Widget.run / Widget.shadowed).
- README updated in 12c205d.
- CodeRabbit: 5 threads resolved by CodeRabbit after 12c205d; test-only `.iter().find()` thread resolved by me with reasoning (PERFORMANCE heading; guardrail detection excludes tests).
- Follow-ups: #879 #880 #881 #882.

### Final fresh review
Spawned fresh code-reviewer on 12c205d (guardrail independent-final-review-for-prs). Pending.
