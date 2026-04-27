# LSP Enrichment Stuck Release Blocker

## Problem Statement
**Updated:** 2026-04-26

**Current framing:** We tried `/dead-code`, saw noisy candidates, and considered whether to improve the skill or pull broader bootstrap-DX work from #646 into v0.2.7.

**Reframed as:** Agents need RNA readiness signals to fail closed because release-critical skills like `/dead-code` depend on completed LSP call/reference edges, but current main can leave LSP enrichment in `RUNNING` for 13+ hours with no completion, failure, or stale-state transition.

**The shift:** This is not primarily a dead-code cleanup problem or a full bootstrap-DX redesign. It is a bounded-lifecycle and truth-in-readiness problem: if RNA cannot tell agents that graph enrichment is incomplete or stuck, agents will make confident claims from degraded graph data.

### Constraints
- **Hard:**
  - `/dead-code` must not emit candidates unless LSP call/reference enrichment is complete enough for the target language/scope.
  - MCP-visible readiness must represent the real state; `RUNNING` cannot remain indefinitely without becoming complete, failed, or stale/stalled.
  - v0.2.7 should not ship a release recommendation that depends on dead-code readiness while installed main reports `LSP: enriching` for many hours.
  - Preserve the event-driven extraction/enrichment architecture; do not reintroduce foreground blocking as the default MCP startup path.
- **Soft:**
  - Internal LSP implementation can still have ordered stages, but user-facing logs/status should not imply the old pass-oriented architecture.
  - Full #646 scope (`scan --no-lsp`, `--no-embed`, `--extract-only`, `rna enrich`, full `--timings`) can remain a follow-up unless a narrow piece is needed to fix readiness truth.

### Evidence
- Installed CI build of main after PRs #653-#656.
- After MCP restart, `search(... verbose=true)` reported `LSP: enriching (48596s)`.
- `list_roots` showed json/markdown/protobuf LSP entries but no completed rust LSP entry.
- `/tmp/rna-mcp-debug.log` showed rust LSP started at `2026-04-26T21:12:52`, reached `8465/10875 nodes`, then did not log `LSP Pass 1 complete`, `LSP enrichment complete for rust`, or `[cache-hit bus] LSP enrichment complete` for the current run.
- Processes remained alive after ~13.5h: MCP active, `rust-analyzer` idle at 0% CPU.

### What this framing enables
- Add bounded timeout/stale detection around background/cache-hit LSP enrichment.
- Make `list_roots` and search footers truthfully surface complete/failed/stalled readiness.
- Update `/dead-code` to abort when required LSP readiness is not complete.
- Rename or clarify user-facing "Pass" terminology as LSP stages/events, without needing to redesign the whole event bus.

### What this framing excludes
- Removing the dead-code skill from the release.
- Treating dead-code cleanup candidates as release blockers.
- Pulling all of #646 into v0.2.7 unless the narrow readiness fix requires it.

## Proposed Release-Blocker Issue

### Title
Release blocker: LSP enrichment can remain RUNNING indefinitely, making dead-code readiness unsafe

### Acceptance Criteria
- LSP enrichment status has a bounded lifecycle for MCP/background/cache-hit-bus paths: it transitions from `RUNNING` to `COMPLETE`, `FAILED`, or an explicit stale/stalled state when progress stops or a stage exceeds its budget.
- Rust LSP diagnostics/reference/call hierarchy requests cannot keep the enrichment task alive indefinitely; hung requests are timed out, logged, counted, and surfaced.
- `list_roots` and search footer expose the degraded/stalled state clearly enough for agents to avoid relying on incomplete LSP edges.
- `/dead-code` documentation/skill flow fails closed when target LSP readiness is not complete; it must not emit candidates from structural-only or currently-enriching graphs.
- User-facing logs/status stop presenting the event-driven pipeline as generic multi-pass execution; if ordered work remains inside the LSP consumer, call it stage-specific internal work.
- Add regression coverage for the lifecycle transition/stalled readiness behavior, plus a manual MCP verification note using the installed binary.

## Links
- Release-blocker issue: #657 — https://github.com/open-horizon-labs/repo-native-alignment/issues/657
- Related broader DX issue: #646

## Problem Space
**Updated:** 2026-04-26

## Problem Space Map

**Date:** 2026-04-26
**Scope:** Release-blocking readiness failure around LSP enrichment, `/dead-code`, and agent-visible graph trustworthiness.

### Objective
We are optimizing for: agents can safely decide which RNA graph capabilities are trustworthy before using them, especially release-facing skills that depend on LSP-derived call/reference edges.

Success is not "dead-code finds fewer false positives" or "LSP always finishes fast." Success is: RNA never lets an agent mistake an incomplete or stalled graph for a complete one, and the release process can distinguish degraded-but-usable graph navigation from capabilities that must fail closed.

### Constraints

| Constraint | Type | Reason | Question? |
|------------|------|--------|-----------|
| Agent-visible readiness must be truthful | hard | RNA's core purpose is letting agents discover local context without guessing | No |
| `/dead-code` requires completed call/reference edges for its target scope | hard | Without LSP `Calls`/`ReferencedBy`, structural edges make most functions look dead | No |
| MCP startup should remain non-blocking/event-driven | hard | Background LSP was intentionally introduced so agents get an initial graph quickly | No, but completion semantics must be bounded |
| New readiness/stall metadata must be delivered through MCP output, not only computed | hard | Guardrail: computed-but-not-delivered | No |
| Current event bus vocabulary includes `PassesComplete` and LSP logs say `Pass N` | soft | Historical naming survived the event-bus migration | Yes: rename or clarify user-facing terminology without redesigning everything |
| Full #646 bootstrap DX scope belongs in this release | assumed | Tempting because the symptom overlaps with deferrable LSP/embeddings | Yes: likely too broad for v0.2.7; keep only the readiness slice |
| Dead-code cleanup candidates should block release | assumed | The skill surfaced plausible orphan clusters | Yes: cleanup can follow; unsafe readiness cannot |
| Rust analyzer behavior is the only problem | assumed | The observed stuck run is Rust, but the architecture applies to all LSP consumers | Yes: fix lifecycle at the generic status/orchestration boundary where possible |

### Terrain
- **Systems:** MCP server pre-warm/background enrichment, EventBus extraction pipeline, `LspConsumer`, Rust LSP transport and diagnostics/reference stages, `LspEnrichmentStatus`, `list_roots`, search footer rendering, `/dead-code` skill documentation, LanceDB sentinel/cache-hit bus path.
- **Stakeholders:** agents using MCP tools, release agent evaluating v0.2.7, maintainer deciding GO/NO-GO, future users running RNA on medium/large repos.
- **Blast radius:**
  - If understated: agents emit dead-code findings from degraded structural-only graphs and may delete live code.
  - If overstated: release scope balloons into #646 and delays a useful release for CLI DX work that can follow.
  - If fixed only in logs: status remains untrustworthy through MCP, violating computed-but-not-delivered.
  - If fixed by blocking startup: reverses the event-driven/non-blocking MCP design and hurts first-use navigation.
- **Precedents:**
  - `docs/lsp-enrichment.md` says LSP consumers fire `EnrichmentComplete` and that a 10-minute circuit breaker applies during indexing readiness; the observed 13+ hour `RUNNING` state violates the spirit of that contract after enrichment starts.
  - `.oh/metis/dead-code-skill-rationale.md` states dead-code is a graph query skill, not a Rust extraction pass, and must abort if LSP did not complete.
  - `.oh/guardrails/computed-but-not-delivered.md` requires any readiness/stall state to appear in actual MCP outputs, not just internal logs.
  - Existing code already has `LspEnrichmentStatus` footer/list_roots tests, so the status surface is the natural delivery point.

### Assumptions Made Explicit
1. We assume the stuck state is enough to block v0.2.7 even if other release checks were green. If false: we can ship v0.2.7 while explicitly excluding `/dead-code` readiness from release notes and make #657 follow-up.
2. We assume agents will use `/dead-code` because we just made it discoverable. If false: this is less urgent, but still a context-assembly trust bug.
3. We assume the right fix is bounded lifecycle/status truth, not simply killing `rust-analyzer`. If false: operational restart guidance might be enough, but that would leave future agents exposed.
4. We assume internal ordered LSP stages are acceptable if surfaced honestly. If false: the event-bus migration has deeper architectural debt and #657 is too narrow.
5. We assume #646's phase controls are not required to fix the release blocker. If false: a minimal `skip/defer` mechanism may need to come forward, but only as needed for readiness truth.

### X-Y Check
- **Stated need (Y):** Improve `/dead-code`, maybe scope in #646, and address the stuck `LSP: enriching` run.
- **Underlying need (X):** Preserve agent trust in RNA capability readiness: agents need to know whether LSP-dependent graph queries are safe, degraded, failed, or still unavailable.
- **Confidence:** High that Y is a symptom of X. The dead-code false-positive risk and #646 bootstrap concern both point to the same capability-readiness boundary.

### Ready for Solution Space?
Yes, with a narrower release-blocker scope than full #646. Explore solutions that make enrichment lifecycle bounded and MCP-visible, make `/dead-code` fail closed on incomplete/stalled LSP, and clarify pass/stage terminology. Do not scope broad CLI phase controls or dead-code cleanup into v0.2.7 unless they directly support that readiness boundary.
