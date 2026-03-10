# Session: initial-dx

## Aim
**Updated:** 2026-03-09

**Aim:** A developer who discovers repo-native-alignment on GitHub can install it, run it against their repo, and get value from it within 15 minutes — without hitting a platform error, a missing license, or a "how do I even query this?" moment.

**Current State:** A developer finds the repo, clones it, runs `cargo build --release`... and:
- CI is red (ubuntu-latest runner, but Metal GPU deps are macOS-only) — first impression is "broken"
- No LICENSE file — cautious devs won't touch it
- The only query interface is MCP — you need Claude Code wired up before you can ask a single question
- README says "validated on 3 repos" but doesn't say which platforms or harnesses
- No way to just `rna search "auth"` from the terminal to kick the tires

**Desired State:** Developer finds repo, downloads a prebuilt binary from GitHub Releases, runs `rna search "auth" --repo .`, sees results, thinks "this is useful", then wires up MCP for the full experience. CI is green. License is clear. Platform constraints are documented upfront.

### Mechanism

**Change:** Five concrete deliverables:
1. **Fix CI** — either switch to `macos-latest` runner or make Metal deps conditional so Linux builds work (or both)
2. **Add MIT LICENSE** — file + Cargo.toml field
3. **Add CLI query interface** — `repo-native-alignment search` / `repo-native-alignment graph` subcommands that exercise the same query engine without MCP
4. **Platform documentation** — README section stating: macOS Apple Silicon only (tested), these harnesses (Claude Code, Oh-My-Pi), these repo types (Rust, Python/TS monorepos, Rust/TS)
5. **GitHub Releases** — CI workflow that builds macOS ARM binary and publishes to GitHub Releases on tag push. This is the primary install path for first-time users.

**Hypothesis:** The biggest barrier to adoption isn't features — it's the "first 10 minutes" experience. A red CI badge, missing license, and no way to try without MCP setup all signal "not ready." Fixing these signals converts curious developers into actual users. Prebuilt binaries eliminate the 3+ minute cargo compile barrier entirely.

**Assumptions:**
- Metal GPU dependency is what breaks the Linux build (candle-core with metal feature)
- CLI query is low effort because the query engine already exists in src/query.rs
- MIT is acceptable (confirmed)
- macOS ARM (Apple Silicon) is the only release target for now

### Feedback

**Signal:** CI badge is green, a new developer can follow README → download binary → first query result without asking for help
**Timeframe:** Immediate — this is a checklist, not an experiment

### Guardrails
- **Don't add Linux GPU support** — just make it compile (CPU-only fallback or macOS-only CI)
- **Don't build a full CLI REPL** — just expose the existing query/graph functions as subcommands
- **Keep README honest** — state what's tested, don't overclaim
- **GitHub Releases: macOS ARM only for now** — don't promise Linux/Windows binaries until tested

### Execution Order
1. LICENSE (trivial)
2. CI fix (unblocks green badge)
3. GitHub Releases workflow (primary install path)
4. Platform docs (README update)
5. CLI query interface (the real work)

---

## Problem Space
**Updated:** 2026-03-09

### Objective
Minimize time-to-first-value: "found on GitHub" → "useful output from my repo."

### Constraints

| Constraint | Type | Reason | Question? |
|------------|------|--------|-----------|
| Rust binary (~200MB) | hard | LanceDB + candle + tree-sitter | No, but prebuilt binary hides compile time |
| Metal GPU for embeddings | soft | candle-core `metal` feature | CPU fallback already exists in code (embed.rs:35-38). Make feature conditional in Cargo.toml |
| No npm/uvx golden path | hard | Rust binary, not JS/Python | npm wrapper (esbuild pattern) possible but heavy — defer |
| `setup` command does 6 things | soft | Installs binary + skills + .mcp.json + AGENTS.md + source boundary + verify | Could split; teach-oh could bootstrap |

### Terrain

**MCP install friction hierarchy** (ecosystem research):
1. `npx -y @scope/server` — zero install (JS golden path)
2. `uvx mcp-server-x` — zero install (Python)
3. Docker image — one prereq
4. `cargo install` from crates.io — needs Rust toolchain
5. GitHub Releases binary download — needs curl/browser
6. Clone + `cargo build --release` — 3+ min compile, needs Rust

**Where RNA sits today:** Level 6. **Target:** Level 5 (GitHub Releases) primary, plus Claude Code marketplace entry for MCP users.

**The real golden path for MCP:** `claude mcp add` from a registry. The MCP Registry at modelcontextprotocol.io is the discovery layer. Claude Code also has `claude mcp add <name>` which writes config directly. This is likely where most MCP-aware users will find and install servers.

**Two user journeys:**

**Journey A: Evaluator** — "Let me try this"
```
GitHub README → download binary (or claude mcp add) → rna search "auth" --repo . → "useful" → full MCP setup
```
Needs: GitHub Releases + CLI query + clear README + registry listing.

**Journey B: Adopter** — "Set up my project"
```
Has binary → rna setup --project . (or teach-oh detects missing RNA) → working MCP + .oh/ + AGENTS.md
```
Already works. teach-oh could bootstrap binary install if RNA not found.

### Install Path: What's Normal for MCP?

| Server | Language | Install method | Config |
|--------|----------|---------------|--------|
| filesystem | TypeScript | `npx -y @modelcontextprotocol/server-filesystem` | inline in .mcp.json |
| github | Go | Docker image OR GitHub Releases binary | `docker run` or binary path |
| git | Python | `uvx mcp-server-git` | inline |
| cargo-mcp | Rust | `cargo install cargo-mcp` | binary path |
| **RNA (target)** | Rust | **GitHub Releases binary** + `claude mcp add` | binary path + `--repo` arg |

For Rust MCP servers, the pattern is: **GitHub Releases for binaries, crates.io for Rust users, registry listing for discovery.** No npm wrapper needed at this stage.

### Metal / Linux: Clean Conditional Path

`metal_candle` and `candle-core` with `metal` feature are only used in `src/embed.rs`. The code already has a CPU fallback (line 35-38: `Device::new_metal(0).unwrap_or_else(|_| Device::Cpu)`). The fix:
- Add cargo feature `metal` (default on macOS)
- Gate `candle-core` metal feature and `metal-candle` dep behind it
- CI stays on `ubuntu-latest` (CPU-only build)
- GitHub Releases builds both: macOS ARM (with Metal) + Linux x86_64 (CPU-only)

### teach-oh as Bootstrap

Current flow: install binary → run `setup` → run `/teach-oh`

Better flow: user opens Claude Code in their project → runs `/teach-oh` → teach-oh detects RNA not installed → offers install (download from latest release or `cargo install`) → configures .mcp.json → continues with strategic setup.

This makes teach-oh the single entry point for adopters. The `setup` subcommand remains for CI/scripted use.

### Assumptions Made Explicit
1. Claude Code marketplace / MCP registry listing is achievable for v0.1 — if false: GitHub Releases is still the primary path
2. CPU-only Linux build is useful enough without Metal GPU — if false: semantic search is slow but still works; all structural tools unaffected
3. teach-oh can detect and install binaries — if false: keep setup as separate step, document clearly
4. ~200MB binary is acceptable for download — if false: need strip/UPX or reduce deps

### X-Y Check
- **Stated need (Y):** Fix CI, add license, add CLI, add releases, add docs
- **Underlying need (X):** Make the tool installable and evaluable by someone who doesn't already know it works
- **Confidence:** High — Y directly serves X, with the addition of registry listing and Linux build

### Ready for Solution Space?
**Yes.** The terrain is mapped. Key refinements from problem space exploration:
- **Add Linux x86_64 CPU-only build** to releases (trivial via cargo feature flag)
- **Target Claude Code MCP registry** as the discovery/install path alongside GitHub Releases
- **Consider teach-oh bootstrap** as a stretch goal for seamless adopter experience
- Execution order updated below

### Revised Execution Order
1. LICENSE + Cargo.toml metadata (trivial)
2. **Flip repo public** — LICENSE unblocks this; public unblocks everything else (Releases, marketplace, discovery)
3. Cargo feature gate for Metal (enables Linux build)
4. CI fix (stays ubuntu-latest, builds CPU-only)
5. GitHub Releases workflow (macOS ARM + Linux x86_64 on tag push)
6. CLI query subcommands (search, graph — evaluator journey)
7. README update (platform docs, install-from-release, tested harnesses, Claude marketplace install path)
8. (Stretch) teach-oh binary bootstrap

### Install Paths (final)
- **Claude Code marketplace:** `claude mcp add-from-github open-horizon-labs/repo-native-alignment` — pulls from GitHub Releases, no registry needed
- **GitHub Releases:** direct binary download for CLI evaluators
- **cargo install:** for Rust developers who prefer source builds
- **teach-oh bootstrap:** (stretch) detect missing binary, guide install

---

## Solution Space
**Updated:** 2026-03-09

### #91 — MIT LICENSE + Cargo.toml metadata
**What:** LICENSE file (MIT, copyright Open Horizon Labs 2025-present) + `license = "MIT"` in Cargo.toml.
**Success:** GitHub renders "MIT License" in sidebar. `cargo metadata` reports license.

### Flip repo public
**What:** Change repo visibility to public on GitHub. Gate: LICENSE must exist first.
**Success:** Repo is publicly accessible. Unblocks GitHub Releases, marketplace install, external discovery.

### #92 — Cargo feature gate for Metal
**What:** Add `[features]` to Cargo.toml: `default = ["metal"]`, `metal = ["dep:metal-candle", "candle-core/metal"]`. Make `metal-candle` an optional dep. `#[cfg(feature = "metal")]` in embed.rs for Metal-specific imports. CPU-only path when feature absent.
**Approach:** `default = ["metal"]` — current macOS users see no change. Linux CI/releases build with `--no-default-features`.
**Success:**
- `cargo build --release` on macOS works identically (Metal GPU)
- `cargo build --release --no-default-features` compiles on Linux (CPU-only)
- `cargo test --no-default-features` passes all tests

### #93 — Fix CI
**What:** Update `.github/workflows/rust-main-merge.yml` — all cargo commands pass `--no-default-features`. Stays on `ubuntu-latest`.
**Success:** CI pipeline green. All tests pass. Smoke test passes on Linux.

### #94 — GitHub Releases workflow
**What:** New `.github/workflows/release.yml`. Trigger: `v*` tags. Matrix: macOS ARM (default features/Metal) on `macos-latest`, Linux x86_64 (`--no-default-features`) on `ubuntu-latest`. Strip binaries. `gh release create` with both attached.
**Binary names:** `repo-native-alignment-darwin-arm64`, `repo-native-alignment-linux-x86_64`
**Success:**
- `git tag v0.1.0 && git push --tags` produces a release with two binaries
- macOS binary uses Metal GPU
- Linux binary uses CPU
- `claude mcp add-from-github` can pull the binary

### #95 — CLI query subcommands
**What:** Add `Search` and `Graph` subcommands to `Commands` enum in main.rs.
- `search`: wraps search_symbols (structural) or oh_search_context (with `--semantic`). Args: `query`, `--repo`, `--kind`, `--language`, `--file`, `--limit`, `--semantic`, `--include-code`, `--include-markdown`.
- `graph`: wraps GraphIndex::neighbors/impact/reachable. Args: `--node`, `--mode`, `--direction`, `--edge-types`, `--max-hops`, `--repo`.
- Output: print existing markdown output to stdout (reuse MCP tool formatting). Add `--json` later if needed.
- Both init scanner/graph on first use (same pipeline as MCP startup, one-shot).
**Success:**
- `repo-native-alignment search "embed" --repo .` returns symbols in <10s
- `repo-native-alignment graph --node <id> --mode impact --repo .` returns dependents
- Works without MCP, without `.oh/`, without prior setup
- `--help` is clear

### #96 — README update
**What:** Add Platform Support section (macOS ARM full, Linux x86_64 CPU-only, Windows untested). Rewrite Install to lead with GitHub Releases download + `claude mcp add-from-github`. Add Tested On section (Claude Code + Oh-My-Pi; Rust, Python/TS monorepo, Rust/TS). Add CI badge. Add License mention.
**Success:** New user follows README from zero to first query without external help. Platform constraints visible before install attempt.

### #97 — teach-oh binary bootstrap
**Repo:** `open-horizon-labs/skills` (../skills locally)
**What:** Update teach-oh SKILL.md to detect missing RNA binary and bootstrap install before continuing.
**Detection logic (added to teach-oh's exploration phase):**
1. Check if `repo-native-alignment` is on PATH (`which repo-native-alignment`)
2. Check if `.mcp.json` references `rna-server` or `repo-native-alignment`
3. If neither: offer to install from latest GitHub Release for current platform
**Install flow:**
- Detect platform (macOS ARM vs Linux x86_64)
- Download binary from `https://github.com/open-horizon-labs/repo-native-alignment/releases/latest`
- Place in `~/.local/bin/` (or user-specified location)
- Run `repo-native-alignment setup --project .` to configure .mcp.json
- Continue with normal teach-oh flow
**Success:**
- `/teach-oh` in a project without RNA → detects gap → offers install → installs → configures → continues
- Works on macOS ARM and Linux x86_64
- Non-destructive: if user declines, teach-oh continues without RNA (degraded but functional)
