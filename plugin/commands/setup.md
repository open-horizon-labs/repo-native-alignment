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

**If neither:** Tell the user their platform is not yet supported. They can build from source:
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
## Code Exploration (use RNA MCP tools, not grep/Read)

| Instead of... | Use this MCP tool |
|---|---|
| `Grep` for symbol names | `search_symbols(query, kind, language, file)` |
| `Read` to trace function calls | `graph_query(node_id, mode: "neighbors")` |
| `Grep` for "who calls X" | `graph_query(node_id, mode: "impact")` |
| `Read` to find .oh/ artifacts | `oh_search_context(query)` |
| `Bash` with `grep -rn` | `search_symbols` or `oh_search_context` |
| Recording learnings/signals | Write to `.oh/metis/`, `.oh/signals/`, `.oh/guardrails/` (YAML frontmatter + markdown) |
| Searching git history | `oh_search_context(query)` — returns hash; use `git show <hash>` via Bash for diffs |
<!-- end RNA MCP tool guidance -->
```

If AGENTS.md doesn't exist, offer to create it with the tool guidance block as the initial content. Ask: "No AGENTS.md found. Create one with RNA tool guidance?" If accepted, write the block above as the file content.

## Step 6: Inform the user

Tell the user:
1. Setup is complete
2. They need to **restart Claude Code** for the MCP to load
3. After restart, RNA MCP tools will be available:
   - `oh_get_context` - Business context in one call
   - `oh_search_context` - Semantic search over outcomes, code, commits
   - `search_symbols` - Multi-language symbol search with graph edges
   - `graph_query` - Impact analysis, neighbor traversal, reachability
   - `outcome_progress` - Structural outcome-to-code joins
   - `list_roots` - Workspace root management
4. Optional: run `repo-native-alignment setup --project .` for full OH Skills + agents setup
