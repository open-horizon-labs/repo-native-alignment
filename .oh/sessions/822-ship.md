## Ship Pipeline — PR #822

**Started:** 2026-07-21
**Reviewed head:** `2fd9d2fee77783b45a9b40814fee2c195ad055f9`

### Pre-flight

- PR #822 batches and closes #810–#814 from `issue/810`.
- Worktree index is live with 52,612 extracted symbols.
- LSP caller/reference coverage is explicitly degraded for Python and TypeScript; exact graph/source review remains available and no conclusion below relies on complete LSP coverage.
- Existing external review contains only CodeRabbit's draft-skip boilerplate; CodeRabbit is advisory and is not being triggered or awaited.

### Step 1: RNA-Grounded Review

**Verdict:** CONTINUE

- Full #810–#814 acceptance matrix posted at <https://github.com/open-horizon-labs/repo-native-alignment/pull/822#issuecomment-5037808700>.
- Relevant metis/guardrails are honored structurally.
- RNA impact confirms one shared service entry for CLI/MCP/viewer dispatch.
- Six concrete high-risk concerns were checked; no unresolved actionable finding remains.

### Step 2: Independent Code Review

**Reviewer:** `/root/ship_822/pr822_step2_review`
**Reviewed commit:** `2fd9d2fee77783b45a9b40814fee2c195ad055f9`
**Verdict:** REQUEST CHANGES

- Review posted at <https://github.com/open-horizon-labs/repo-native-alignment/pull/822#issuecomment-5037920136>.
- Four P1 findings: legacy traversal control bypass, token-only task admission, incomplete live diff relationship/behavior inference, and missing comparative outcome assertions.
- No ship edit was made before reporting the bounded findings to the root implementer for the authorized remediation decision.

### Step 3: Consolidated Ship Remediation

- Legacy node/nodes/traversal/target-subsystem dispatch now rejects explicit product projection/body/budget/task/delta controls while a direct regression proves default legacy bytes/order are unchanged.
- Task admission converts token-only bounds to the deterministic `chars / 4` estimate's byte currency and uses the tighter byte/token bound; the final canonical renderer remains authoritative.
- Evidence projection reports exact coalesced per-span bytes/chars/token estimate without adding audit data to the agent projection.
- Live graph-delta consumes canonical proposal-grounded call/reference/registration/state facts, uniquely corroborates endpoints, emits five grounded behavior classes, and discovers only the nearest affected test layer.
- Two real RNA task fixtures now cover four explicit obligations at lower rendered cost than a larger flat top-k; the real RNA graph-delta fixture is smaller and more role-specific than the source-body neighbors/impact views required for every covered locus.
- The default/compact/evidence matrix and the truthful non-measured four-query channel diagnostic close the remaining review evidence gaps.

### Focused Verification

- `cargo check --lib --no-default-features`: pass.
- `cargo test --lib --no-default-features service::search`: 183 passed, 0 failed.
- Rust 2024 formatting and `git diff --check`: pass.
- The first comparative fixture execution correctly failed because the baseline already covered the requested test and because a one-seed body-free traversal was not context-equivalent. The fixture-only correction now uses exact named tests plus dependency/impact obligations, and compares the graph card with all source-bearing raw views needed to reproduce it.

### RNA Tool Friction Log

| Time | Tool path | Severity | Friction | Impact / response |
|---|---|---:|---|---|
| 2026-07-21 | Ship metis prerequisite | low | `.claude/agents/ship.md` names `.oh/metis/computed-but-not-delivered.md`, while the current canonical artifact is `.oh/guardrails/computed-but-not-delivered.md`. | RNA artifact search found and supplied the canonical hard guardrail; no code-navigation fallback was used. |
| 2026-07-21 | RNA exact-symbol search | low | Several exact new function-name queries returned empty despite the live index. | Used RNA's file-scoped function inventory and graph-neighbor queries; no raw source fallback was needed. |
| 2026-07-21 | Remediation integration inspection | low | The live RNA snapshot exposed signatures but lagged the uncommitted remediation bodies. | Used only the exact four-file current diff and one RNA-located bounded `search.rs` range to verify the combined adapter/test patch. |
| 2026-07-21 | Rustfmt probe | low | An initial direct `rustfmt --edition 2021 --check` rejected pre-existing Rust 2024 let-chains. | Re-ran the owned files with the repository's Rust 2024 edition; formatting passed without source changes. |
