# Session: install-init

## Aim
**Updated:** 2026-03-07

**Aim:** New operators can go from zero setup to a first successful aligned agent session in under 10 minutes, instead of manually wiring RNA MCP, OH MCP, and skills across multiple repos.

**Current State:** Setup requires separate installs (skills, RNA build, MCP config edits, init steps), manual path wiring in `.mcp.json`, and cross-doc hopping; failures are common and discovered late.
**Desired State:** Operators run one install+init path that configures RNA MCP + OH MCP + required skills (including Miranda skills), validates connectivity, and lands in a working session immediately.

### Mechanism
**Change:** Provide a single idempotent bootstrap flow (install + init + verify) that installs/updates required components, writes/merges MCP config safely, initializes repo context, and runs a real-client smoke check proving tools are callable.
**Hypothesis:** Most adoption drop-off is setup friction, not concept friction; collapsing setup into one deterministic path will increase successful first sessions and reduce abandonment.
**Assumptions:**
- Users accept one opinionated bootstrap entrypoint.
- Environment prerequisites can be detected and surfaced with actionable errors.
- Real-client verification catches most “looks installed but doesn’t work” failures.

### Feedback
**Signal:** Median time to first successful tool call < 10 minutes; first-run success rate without manual config edits ≥ 80%.
**Timeframe:** First 10-20 installs, then 2-week follow-up.

### Guardrails
- No hidden mutable state outside repo/user config; all changes explicit and reversible.
- Verification must use a real MCP client path, not transport-only probes.
- Keep repo-native and lightweight.
- Failure paths must be actionable.

## Problem Statement
**Updated:** 2026-03-07

**Current framing:** “Install is too painful because it requires cloning the repo, compiling, and manual setup.”

**Reframed as:** Developer users need a deterministic, low-friction path to first successful use of RNA MCP + OH MCP + skills because setup failure blocks adoption; currently bootstrapping is a manual, multi-system process with late, hard-to-diagnose failures.

**The shift:** From a packaging complaint (“how do we install?”) to an onboarding reliability problem (“how do we guarantee first-run success quickly?”). X-Y: requested Y = “better install flow”; underlying X = “predictable, verifiable time-to-first-working-session.”

### Constraints
- **Hard:** Must remain repo-native/lightweight, match real MCP client behavior, and avoid destructive config overwrite.
- **Soft:** Manual source build as default, docs-only setup, and “devs can tolerate pain” assumptions.

### What this framing enables
- Compare delivery mechanisms without locking in prematurely.
- Treat preflight/config merge/smoke verification as core behavior.
- Measure by first-run completion and time-to-first-tool-call.

### What this framing excludes
- Doc-only cleanup without reducing failure modes.
- Unverified “installed” states where binary exists but session still fails.


## Solution Space
**Updated:** 2026-03-07

### Analysis

**Problem:** Developer users cannot reliably reach a first working RNA+OH+skills session quickly because setup is still a manual multi-step flow (build, config wiring, dependency discovery, verification).
**Key Constraint:** Keep setup repo-native/lightweight and verify with real MCP client behavior, not transport-only checks.

### Candidates Considered

| Option | Level | Approach | Trade-off |
|--------|-------|----------|-----------|
| A | Band-Aid | Improve docs only (explicit `protoc`, `cargo install --path .`, `.mcp.json` examples, troubleshooting) | Fastest, but failure still user-driven and late-discovered |
| B | Local Optimum | Distribute installable binaries (`cargo install`/release artifacts) and standardize paths | Easier install, but still no deterministic init/config/verification |
| C | Reframe | Add one idempotent bootstrap command (`install + init + verify`) that preflights deps, installs tools, merges MCP config, and smoke-tests via real client path | Moderate implementation effort; needs careful cross-platform behavior |
| D | Redesign | Build a first-class setup workflow in the RNA CLI (`repo-native-alignment setup`) with pluggable installers for RNA, OH MCP, and skills | Best long-term UX, highest upfront complexity and maintenance |

### Evaluation

**Option A: Docs-only hardening**
- Solves stated problem: **Partially**
- Implementation cost: **Low**
- Maintenance burden: **Low-Medium**
- Second-order effects: Repeats “read docs then debug locally” pattern; first-run success remains inconsistent

**Option B: Packaging-first distribution**
- Solves stated problem: **Partially**
- Implementation cost: **Medium**
- Maintenance burden: **Medium**
- Second-order effects: Reduces compile friction but leaves config merge, skills wiring, and verification gaps

**Option C: Idempotent bootstrap command**
- Solves stated problem: **Yes**
- Implementation cost: **Medium**
- Maintenance burden: **Medium**
- Second-order effects: Creates a measurable first-run path and catches dependency/config failures early; can evolve into CLI-native setup later

**Option D: Full CLI setup redesign**
- Solves stated problem: **Yes**
- Implementation cost: **High**
- Maintenance burden: **High**
- Second-order effects: Strongest UX, but risks violating `validate-before-building` if built before proving behavior lift

### Recommendation

**Selected:** Option C - Idempotent bootstrap command
**Level:** Reframe

**Rationale:** This directly targets the reframed problem (reliable time-to-first-working-session), stays within hard constraints (`repo-native`, `lightweight`), and provides a concrete measurement surface (success rate/time) without overcommitting to a heavier redesign.

**Accepted trade-offs:**
- Initial implementation will likely be shell-first and require platform guards.
- Packaging improvements (Option B) and CLI-native setup (Option D) may still be needed later, but only after measuring Option C impact.

### Implementation Notes

1. Preflight: check `cargo`, `protoc`, `node/npx`, and required CLIs; fail with exact remediation commands.
2. Install/update: install RNA binary and required skills packages idempotently.
3. Configure: merge `.mcp.json` entries safely (no destructive overwrite), fixing stale absolute paths.
4. Verify: run real-client smoke path (tool call sequence) and emit pass/fail summary.
5. Instrument: record first-run timing + success/failure reason to `.oh/signals/` for adoption feedback.

Local-maximum check: we are intentionally not stopping at docs or packaging-only; we choose the first approach that makes success deterministic and measurable.

## Execute
**Updated:** 2026-03-07
**Status:** complete

### Implementation

Option C is implemented as a native Rust subcommand: `repo-native-alignment setup`.

**Command interface:**
```
repo-native-alignment setup --project <PATH>  # default: .
  --dry-run        print planned actions, make no changes
  --skip-skills    skip OH skills install step
  --skip-verify    skip post-setup verification
```

**Execution flow (in order):**
1. Preflight: checks only the tools required for selected actions (`cargo`/`protoc` when source reinstall is possible, `npx` unless `--skip-skills`); emits remediation and aborts on missing required deps.
2. RNA install: runs `cargo install --locked --path <rna repo root>` when source path exists; if source is missing but an installed binary exists, setup continues using the existing binary.
3. Skills install: `npx skills add open-horizon-labs/skills -g -a claude-code -y` — skipped on `--skip-skills` or `--dry-run`.
4. MCP config merge: reads `<project>/.mcp.json` if present, sets `mcpServers.rna-server` to:
   - `type: "stdio"`
   - `command: "<CARGO_INSTALL_ROOT|CARGO_HOME|HOME cargo bin>/repo-native-alignment"`
   - `args: ["--repo", "<absolute project path>"]`
   - `timeout: 10000`
   Preserves all other top-level keys and server entries. Writes pretty JSON output.
5. Verify: binary `--help` check + `.mcp.json` `rna-server` presence check; prints pass/fail summary.

**Documentation updated:**
- README Quick Start rewritten to lead with `repo-native-alignment setup --project .`.
- Prerequisites table added (cargo, protoc, npx).
- Dry-run usage documented.
- Manual/fallback path retained under a brief `---` separator.

### Known Gaps

- OH MCP server installation is **not** automated by `setup`; OH MCP requires a separate install or manual `.mcp.json` entry. The setup command configures RNA MCP only.
- Windows binary naming/path edge cases remain (`repo-native-alignment.exe` and default `%USERPROFILE%\\.cargo\\bin` handling need explicit validation).
- If both RNA source path and installed binary are unavailable, setup fails with remediation (re-clone or install a release binary first).
- Smoke verification (`--skip-verify` bypass) uses binary `--help` rather than a real MCP tool call; true end-to-end verification would require a real MCP client path (noted in guardrails; deferred to P2).


## Ship
**Updated:** 2026-03-07
**Status:** verified

- PR #6 merged to `main` via squash: https://github.com/open-horizon-labs/repo-native-alignment/pull/6
- Delivery path completed: branch -> PR review/dissent -> merge-conflict resolution -> merge -> main
- Added GitHub workflow `.github/workflows/rust-main-merge.yml` to run `cargo test setup::tests` and `cargo build --release` on PRs to `main` and pushes to `main`.
- Post-merge verification: main head contains setup command, docs updates, and workflow file.
- Delivery-path tax observed: one merge conflict in `README.md` added manual integration time.