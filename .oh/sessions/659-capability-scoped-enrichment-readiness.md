# Capability-Scoped Enrichment Readiness

## Session Metadata

**Updated:** 2026-04-27
**Parent issue:** #659 — https://github.com/open-horizon-labs/repo-native-alignment/issues/659
**Symptom issues:** #645, #646, #657

## Outcome Alignment

**Primary outcome:** `context-assembly`

RNA exists so agents can discover and navigate local codebase context without guessing. Capability-scoped enrichment readiness is directly in scope: if agents cannot tell whether callers, references, diagnostics, embeddings, or dead-code coverage are trustworthy, RNA is assembling plausible context rather than true context.

**Secondary outcomes:**

- `agent-alignment` — agents need MCP-visible readiness to decide what claims they can safely make and when to fail closed.
- `codebase-to-warehouse-pipeline` — diff/change workflows need structured, scoped metadata about what changed; this problem space clarifies that full-repo enrichment is not always the right source of truth.
- `domain-context-compiler` — capability readiness generalizes beyond code: domain extractors, embeddings, and graph relationships also need truthful readiness/degradation semantics.
- `subsystem-detection` — agents should reason about systems/capabilities/subsystems, not undifferentiated global enrichment state.

## Problem Statement

**Current framing:** Three separate issues appear to need separate fixes: #645 embedding progress ambiguity, #646 deferrable first scan, and #657 stuck LSP/dead-code readiness.

**Reframed as:** Agents need workflow-specific graph capabilities with truthful readiness, but RNA currently treats enrichment as coarse global background work. That makes progress ambiguous, first-use slower than necessary, and LSP-dependent skills unsafe because agents cannot tell which metadata is complete, partial, stale, failed, unavailable, or stale relative to the current repo state.

**The shift:** From "make LSP/embedding enrichment finish" to "make capability readiness truthful for the workflow." LSP and embeddings are means to richer graph data, not goals in themselves.

## Problem Space Map

**Date:** 2026-04-27
**Scope:** Enrichment control plane and readiness semantics across LSP, embeddings, extracted graph data, diff workflows, and skill prerequisites.

### Objective

Agents should get trustworthy, fast-enough graph answers for the workflow they are doing.

The objective is not to complete every enrichment phase. The objective is to expose, for each workflow-relevant graph capability, whether it is ready, partial/degraded, running with meaningful progress, failed/stalled, unavailable, stale, or not needed.

### Constraints

| Constraint | Type | Reason | Question? |
|------------|------|--------|-----------|
| Extracted graph must be usable quickly | hard | First-use agent navigation cannot wait on LSP or embeddings | No |
| Capability readiness must be MCP-visible | hard | Agents consume MCP output, not internal logs | No |
| Workflow prerequisites must fail closed | hard | Global dead-code and similar workflows are unsafe without complete enough metadata | No |
| Foreground/background enrichment must not be ambiguous | hard | #645 shows progress/log trust breaks without invocation identity or serialization | No |
| MCP startup must remain non-blocking | hard | Blocking startup would regress the event-driven first-use UX | No |
| Computed readiness must be delivered | hard | `computed-but-not-delivered` guardrail: internal status is insufficient if agents cannot see it | No |
| Existing `scan --full` remains the only obvious control surface | soft | Historical CLI shape; #646 challenges it | Yes |
| LSP is required for all richer metadata | assumed | Tree-sitter, imports, Cargo/check output, tests, docs, and targeted LSP can cover many workflows | Yes |
| One global `LSP: enriching` status is enough | assumed | The observed 13+ hour run disproves this for agent trust | Yes |

### Terrain

- **Systems:** scanner, extracted graph, event bus, LSP consumers, embedding pipeline, LanceDB persistence/sentinels, MCP rendering, CLI scan UX, plugin skills.
- **Stakeholders:** agents using MCP tools, maintainer making release decisions, users scanning new repos, future workflow-specific skills.
- **Blast radius:**
  - If understated: agents trust incomplete metadata and make unsafe claims, e.g. false dead-code candidates.
  - If overstated: release scope balloons into a full durable job system before the problem is shaped.
  - If fixed only in logs: agents still cannot see readiness through MCP.
  - If fixed by blocking startup: initial navigation gets worse and #646 regresses.
- **Precedents/evidence:**
  - #645: embedding progress may reflect overlapping enrichment invocations or ambiguous counters.
  - #646: extract-only graph is already navigable; LSP/embeddings are valuable but deferrable.
  - #657: installed main reported `LSP: enriching (48596s)` after rust LSP stopped making visible progress.
  - `.oh/metis/dead-code-skill-rationale.md`: global dead-code is a graph query that requires LSP call/reference coverage.
  - `.oh/guardrails/computed-but-not-delivered.md`: readiness must be visible where agents consume it.

### Assumptions Made Explicit

1. We assume agents usually need a workflow answer, not maximal enrichment. If false, full-repo enrichment reliability is the dominant problem.
2. We assume diff-review questions can often be answered by extracted graph plus targeted LSP. If false, scoped enrichment underdelivers.
3. We assume overlapping foreground/background enrichment is possible or at least not disprovable from current logs. If false, #645 may be mostly observability, but invocation identity still matters.
4. We assume global dead-code remains useful but heavy. If false, `/dead-code` should be limited to changed-symbol hygiene.
5. We assume capability readiness belongs in core surfaces, not individual skill heuristics. If false, each skill will need bespoke defensive checks.

### X-Y Check

- **Stated need (Y):** Fix stuck LSP enrichment, add scan flags, and clarify embedding progress.
- **Underlying need (X):** Build a truthful enrichment model where agents can request or observe the specific richer metadata needed for a workflow, with bounded progress and explicit degradation.
- **Confidence:** High. The three issues share one missing abstraction: capability-scoped enrichment readiness.

## Issue Relationship

### #645 — embedding progress can reflect overlapping enrichment runs

#645 is the progress-observability symptom. It shows that enrichment work needs invocation identity, caller labels, and enough lifecycle information to distinguish one slow run from overlapping foreground/background runs.

### #646 — first scan should be queryable in seconds

#646 is the bootstrap/control-surface symptom. It shows that extracted graph readiness and enrichment readiness should be separate. A repo should become queryable before LSP and embeddings finish, and users/agents should know which queries are degraded.

### #657 — LSP enrichment can remain RUNNING indefinitely

#657 is the safety/readiness symptom. It shows that global LSP readiness is too coarse and that LSP-dependent skills like global `/dead-code` need complete enough call/reference coverage or must fail closed.

### #659 — parent problem

#659 captures the shared problem: enrichment readiness is coarse, global, and not workflow-specific.

## Fresh Start Direction

Do not continue the abandoned PR #658 implementation path as-is. The next solution-space should compare levels of response against #659:

1. Patch individual symptoms (#645/#657) only.
2. Minimal capability readiness model surfaced through MCP.
3. Scoped enrichment controls for diff/workflow use cases.
4. Full durable enrichment job/control-plane model.

The decision should explicitly split what is required for v0.2.7 from what belongs in follow-up work.

## Salvage Notes From Abandoned #657 Attempt

- A simple `Stalled` state beside coarse `Running` is directionally useful but insufficient.
- `/dead-code` fail-closed behavior is still important, but should be based on capability readiness, not log/string heuristics.
- Targeted tests around MCP-visible readiness remain useful once the readiness model is clarified.
- Broad subagent edits were discarded locally; PR #658 remains a draft remote artifact but should not be treated as the accepted implementation direction.


## Execute
**Updated:** 2026-04-27
**Status:** pre-flight

### Pre-flight Checklist
- [x] Aim is clear — validate whether a tree-sitter/extracted-graph diff overlay can provide useful PR-review metadata without LSP.
- [x] Constraints known — do not build production API, do not require full LSP/embeddings, keep this as a spike, and keep claims grounded in observed PR samples.
- [x] Context loaded — parent issue #659, symptoms #645/#646/#657, and outcome ties are in this session file.
- [x] Scope bounded — evaluate real recent PR diffs against existing extracted symbol ranges; produce a spike report. Do not implement MCP commands or graph storage.
- [x] Success criteria — a report answers whether hunk-to-symbol mapping is promising, what fails, and what should be tested next.

### Spike: tree-sitter diff-to-change-graph feasibility
**Updated:** 2026-04-27
**Status:** complete

#### Hypothesis
A low-latency review overlay can map git diff hunks to current extracted graph symbols using git diff + tree-sitter/RNA line ranges, without running LSP.

#### Method
- Exported current Rust symbol ranges from RNA's cached extracted graph (`repo-native-alignment search --kind ... --language rust`).
- Sampled six recent merged PRs: #648, #649, #653, #654, #655, #656.
- Parsed `git diff --unified=0 <merge>^ <merge>` for each PR.
- For each Rust hunk, mapped current-side line ranges to overlapping extracted symbol ranges in the current graph.
- No LSP queries were used.

#### Results

| PR | Change type | Rust files | Rust hunks | Mapped | Unmapped | Deletion-only |
|---:|---|---:|---:|---:|---:|---:|
| #656 | test-suite shell fix | 0 | 0 | 0 | 0 | 0 |
| #655 | attr_refs / call-site lexer | 2 | 16 | 12 | 4 | 2 |
| #654 | docs/test-suite/dead-code skill | 0 | 0 | 0 | 0 | 0 |
| #653 | ADR validator Rust follow-ups | 5 | 42 | 39 | 3 | 0 |
| #649 | schema extractor regression tests | 2 | 2 | 2 | 0 | 0 |
| #648 | proto extractor | 2 | 34 | 28 | 6 | 2 |

Across sampled Rust hunks: **81/94 mapped to at least one current extracted symbol (86.2%)**. Runtime for diff parsing + line-range mapping was **2.51s** after the current graph/symbol export was available.

#### What Worked
- Implementation hunks usually mapped to useful functions/types/modules.
- Large test additions mapped to `mod tests` plus specific test functions where current symbol ranges existed.
- The approach produced useful review anchors without LSP.
- Non-Rust PRs (#654/#656) naturally become file/artifact-level change graph entries rather than symbol-level Rust entries.

#### Failure / Caution Classes
- Deleted-only hunks cannot map to current symbols without base-side symbol data.
- New top-of-file/import/module-level hunks often do not overlap function/type ranges; they need file/hunk-level representation rather than forced symbol mapping.
- Current-main symbol ranges were used as a rough proxy for per-PR head ranges, so line drift can create false unmapped or false mapped results.
- Broad functions can swallow many hunks; the report needs confidence/provenance, not absolute semantic claims.

#### Finding
The walking skeleton is plausible, but it must be honest: a tree-sitter-only diff overlay can provide fast review context, not perfect semantic deltas. The first implementation should represent unknown/file-level/deleted hunks explicitly instead of pretending every diff maps to a live symbol.

#### Recommended Next Issue
Create a walking-skeleton issue under #659: **Represent a working-tree diff as an ephemeral change graph with extracted-graph impact readiness**.

Acceptance should require changed files/hunks, current symbol overlap mapping, file-level unknowns, deleted-hunk honesty, existing graph context, and readiness gaps. It should explicitly exclude full before/after graph snapshots and LSP.

### Execute Follow-up: Child Issue Created
**Updated:** 2026-04-27
**Status:** complete

Created first walking-skeleton child issue under #659: #660 — https://github.com/open-horizon-labs/repo-native-alignment/issues/660

Title: **Spike /review-readiness skill for diff-scoped graph context**

Purpose: validate the job-shaped skill path before core API work. The issue keeps the scope at skill-level first: use git diff + extracted graph + existing RNA context to report PR/diff review readiness and identify missing capabilities. Core code should follow only if the skill proves useful and exposes hard affordance gaps.

This child issue is intentionally not a generic readiness API issue. It is the concrete PR/diff review use case that should force any later model/API design.

## Execute
**Updated:** 2026-04-27
**Status:** implementation-revised

Implemented #660 walking skeleton in PR #661, then revised it after review showed the helper-script framing failed the user-value test: for docs/skill changes it produced a worse artifact than `git diff`, and it made the skill feel like a fixed report generator rather than an agent process.

### Changes
- Added `plugin/skills/review-readiness/SKILL.md` for `/rna-mcp:review-readiness`.
- Removed the skill-local Python helper; the walking skeleton is now the agent-led review triage process.
- Updated README and AGENTS discoverability entries to describe agent-led triage rather than generated reports.
- Updated `.oh/metis/review-readiness-skill-rationale.md` tying the skill to `context-assembly` and #659.

### Verification
- Confirmed `plugin/skills/review-readiness/review_readiness.py` is no longer present.
- Checked changed-file diff shape with `git diff --stat origin/main...HEAD`.
- Re-read the skill/rationale/docs after edit for coherence.

## HipHi Repo-Family Spike
**Updated:** 2026-04-27
**Status:** evidence gathered

### Testbed
Used `/Users/muness1/src/hiphi-repos` as a repo-family workspace: `roon-knob`, `unified-hifi-control`, `rust-roon-api`, and `hiphi`. Added local testbed config at `/Users/muness1/src/hiphi-repos/.oh/config.toml`:

```toml
[workspace.roots]
unified_hifi_control = "unified-hifi-control"
rust_roon_api = "rust-roon-api"
roon_knob = "roon-knob"
hiphi_site = "hiphi"
```

The scanner requires `[scanner]` for excludes, not `[scan]`, and directory exclude patterns need trailing `/` because the matcher treats slash-suffixed patterns as directory components.

### Scan Result
`repo-native-alignment scan --repo /Users/muness1/src/hiphi-repos` completed with the installed optimized CLI after excludes were fixed:

- Symbols: 9893
- Edges: 28680
- Embeddings: yes
- Time: ~68s

The first attempt exposed a real RNA bug: `json_extractor` sliced `value_text[..500]` and panicked when byte 500 fell inside a multi-byte character. Fixed in PR #661 by truncating on UTF-8 char boundaries.

### Cross-Root Findings
RNA can index the repo family as one workspace and find symbols across repos:

- `unified-hifi-control/src/adapters/roon.rs` imports `roon_api::{ ... transport::{..., Transport, ...} ... }`.
- `rust-roon-api/src/transport.rs` exposes `Transport` and `control`.
- `roon-knob/common/bridge_client.c` and docs show bridge/control surfaces.

But the graph does **not** currently connect the unified import/calls to the sibling `rust-roon-api` provider symbols:

- The unified import has `DependsOn` only to synthetic `roon_api`.
- `rust-roon-api/src/transport.rs:control:function` has no incoming edge from unified-hifi-control.
- `manifest_pass` currently supports package.json, pyproject.toml, requirements.txt, and go.mod, but not Cargo.toml.

### Required Capability
Support a directory hosting a specified library/package:

1. Parse Cargo manifests into package nodes and dependency edges.
2. Record dependency metadata such as package name, git URL, branch/tag, path, and optional alias/rename.
3. In a multi-root workspace, match dependency declarations to local provider roots/packages by package name and repository URL/path.
4. Emit package/root-level `DependsOn` edges across roots before attempting symbol-level call resolution.
5. Use those package edges to resolve imports like `roon_api::transport::Transport` from `unified-hifi-control` to `rust-roon-api` symbols when names line up.

This is the concrete monorepo/repo-family JTBD: diff joins become compelling when a change crosses firmware -> bridge -> local library -> external system boundaries.

## Execute Follow-up: Explicit Package Hosts
**Updated:** 2026-04-27
**Status:** implemented in PR #661

The package-provider relationship should be explicit, not guessed from unique names, repository basenames, or path similarity. Implemented a generic workspace config hook:

```toml
[workspace.package_hosts]
"roon-api" = "rust-roon-api"
```

The resolver is package-manager agnostic at the config level: key = package/library name, value = hosting root or directory. The first parser feeding it is Cargo because the observed HiPhi gap is `unified-hifi-control` depending on `roon-api` hosted by sibling `rust-roon-api`.

### Implemented
- `Cargo.toml` manifest parsing into package nodes and dependency edges.
- Cargo dependency metadata: alias, actual package name, git/path/branch/tag/rev/version.
- Explicit package host resolution from `.oh/config.toml` `[workspace.package_hosts]`.
- Confirmed cross-root `DependsOn` edge only when the package host designation exists.
- Tests that reject implicit unique-name guessing.

### Verification
- `cargo check --lib --no-default-features`
- `cargo check --lib --tests --no-default-features`
- `git diff --check`

Targeted `cargo test --lib --no-default-features manifest::tests` still cannot run on this workstation because the linker cannot find `clang_rt.osx`; test compilation succeeds with `cargo check --tests`.

## Execute Follow-up: Incremental CLI Scan
**Updated:** 2026-04-27
**Status:** implemented in PR #661

The HiPhi verification exposed that plain `repo-native-alignment scan --repo ...` was not actually incremental at the graph/embed layer: scanner detected `0 new, 1 changed, 0 deleted`, but the CLI called `build_full_graph()` and awaited the whole-graph embedding task. This made non-`--full` scans behave like full rebuild/re-embed operations.

### Implemented
- Non-`--full` CLI scan runs `Scanner` first.
- If no files changed, it commits scanner state and loads the cached graph without spawning graph rebuild, LSP, or embedding.
- If cached graph exists and files changed, it calls `update_graph_with_scan` with the pending `ScanResult` so existing targeted changed-file re-embedding is used.
- If no cache exists, it falls back to the foreground initial graph build.

### Verification
- `cargo check --lib --bins --no-default-features`
- `git diff --check`

Local binary execution is blocked by the same workstation linker issue (`clang_rt.osx`) when building a fresh debug binary. The installed CI artifact still has the old behavior, so end-to-end behavior for this fix should be verified from the next CI fast artifact.