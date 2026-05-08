# Setup Repo-Native Alignment MCP

Install the RNA MCP server for aim-conditioned code intelligence.

**Execute these steps in order:**

## Step 1: Check if already installed

```bash
which repo-native-alignment 2>/dev/null
```

If found, skip to Step 3.

## Step 2: Download the binary

Detect the platform and chip, then download to `~/.cargo/bin/` (already on PATH for Rust users):

```bash
OS=$(uname -s)
ARCH=$(uname -m)
CHIP=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo "")
mkdir -p ~/.cargo/bin
```

**If macOS ARM M2+** (`Darwin` + `arm64` + brand_string contains "M2", "M3", or "M4"):
```bash
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-darwin-arm64-fast.tar.gz | tar xz -C ~/.cargo/bin
```

**If macOS ARM (M1)** (`Darwin` + `arm64`):
```bash
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-darwin-arm64.tar.gz | tar xz -C ~/.cargo/bin
```

**If Linux x86_64** (`Linux` + `x86_64`):
```bash
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-linux-x86_64.tar.gz | tar xz -C ~/.cargo/bin
```

**If neither:** Tell the user their platform is not yet supported by the published release artifacts. They may build from source for development/testing, but source builds are not a substitute for release verification from successful CI/release artifacts:
```bash
cargo install --locked --git https://github.com/open-horizon-labs/repo-native-alignment
```

If `~/.cargo/bin` is not on PATH (no Rust toolchain installed), tell the user to add it: `export PATH="$HOME/.cargo/bin:$PATH"`

## Step 3: Add MCP server to Claude Code

RNA is a per-project MCP server (it indexes the repo it's pointed at), so add it with project scope:

```bash
claude mcp add rna-mcp --scope project -- repo-native-alignment --repo .
```

This adds rna-mcp to the project's `.mcp.json` so it activates when working in this repo.

## Step 4: Pre-build the index (recommended)

Run a one-time scan to build the code index before the MCP server starts. This avoids cold-start latency on the first tool call:

```bash
repo-native-alignment scan --repo . --full
```

This builds the full pipeline (scan, extract, embed, LSP enrich, graph) and caches results in `.oh/.cache/lance/`. The MCP server reuses this cache on startup -- if no files changed, graph loads in seconds with zero re-extraction. Subsequent scans are incremental.

Without this step, the MCP server pre-warms the graph automatically at startup, but the first tool call may need to wait for that to complete. Pre-building ensures instant readiness.

## Step 5: Update AGENTS.md with tool guidance

If AGENTS.md exists in the project root, check if it already contains `<!-- RNA MCP tool guidance -->`. If not, append this block:

```markdown
<!-- RNA MCP tool guidance -->
## Code Exploration (RNA MCP)

| Instead of... | Use this RNA MCP tool |
|---|---|
| `Grep` for symbol names | `search(query, kind, language, file)` |
| `Read` to trace function calls | `search(node, mode: "neighbors")` |
| `Grep` for "who calls X" | `search(node, mode: "impact")` |
| `Read` to find .oh/ artifacts | `search(query, include_artifacts=true)` |
| `Bash` with `grep -rn` | `search(query)` — searches code, artifacts, and markdown |
| Codebase orientation | `repo_map(top_n)` |
| Recording learnings/signals | Write to `.oh/metis/`, `.oh/signals/`, `.oh/guardrails/` (YAML frontmatter + markdown) |
| Searching git history | `search(query)` — returns commits; use `git show <hash>` via Bash for diffs |
<!-- end RNA MCP tool guidance -->
```

If AGENTS.md doesn't exist, offer to create it with the tool guidance block as the initial content. Ask: "No AGENTS.md found. Create one with RNA tool guidance?" If accepted, write the block above as the file content.

## Step 6: Inform the user

Tell the user:
1. Setup is complete
2. They need to **restart their agent/IDE** for the MCP server to load
3. After restart, RNA MCP tools will be available:
   - `search` - Symbol search, graph traversal (neighbors/impact/reachable/tests_for/cycles/path), artifact/commit/markdown search. Use `compact: true` to save tokens. Use `mode` + `node` for graph walks.
   - `repo_map` - Codebase orientation: top symbols by importance, hotspot files, entry points, subsystem breakdown
   - `outcome_progress` - Business outcome tracking against code changes
   - `list_roots` - Workspace root management
4. Optional: run `repo-native-alignment setup --project .` for full OH Skills + agents setup
