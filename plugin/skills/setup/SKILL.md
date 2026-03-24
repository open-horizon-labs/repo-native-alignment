---
name: setup
description: Install and configure the Repo-Native Alignment (RNA) MCP server. Downloads the binary, configures the MCP server, pre-warms the code index, and updates AGENTS.md with tool guidance.
---

# Setup Repo-Native Alignment MCP

Install the RNA MCP server for aim-conditioned code intelligence.

**Execute these steps in order. Do not stop between steps or ask for confirmation -- run the full sequence automatically.**

## Step 1: Check if already installed

```bash
which repo-native-alignment 2>/dev/null
```

If found, skip to Step 3. If not found, proceed to Step 2.

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

**If none of the above match:** Tell the user their platform is not yet supported. They can build from source:
```bash
cargo install --locked --git https://github.com/open-horizon-labs/repo-native-alignment
```

If `~/.cargo/bin` is not on PATH (no Rust toolchain installed), tell the user to add it: `export PATH="$HOME/.cargo/bin:$PATH"`

## Step 3: Configure the MCP server

RNA is a per-project MCP server (it indexes the repo it's pointed at).

Check if `.mcp.json` exists in the project root and already contains an `rna-mcp` entry. If it does, skip this step.

If the agent supports `claude mcp add` (Claude Code):
```bash
claude mcp add rna-mcp --scope project -- repo-native-alignment --repo .
```

Otherwise, create or update `.mcp.json` in the project root with:
```json
{
  "mcpServers": {
    "rna-mcp": {
      "command": "repo-native-alignment",
      "args": ["--repo", "."]
    }
  }
}
```

If `.mcp.json` already exists with other servers, merge the `rna-mcp` entry into the existing `mcpServers` object -- do not overwrite the file.

## Step 4: Pre-warm the code index

Run a full scan to build the code index before the MCP server starts. This avoids cold-start latency on the first tool call:

```bash
repo-native-alignment scan --repo . --full
```

This builds the full pipeline (scan, extract, embed, LSP enrich, graph) and caches results in `.oh/.cache/lance/`. The MCP server reuses this cache on startup -- if no files changed, graph loads in seconds with zero re-extraction. Subsequent scans are incremental.

Without this step, the MCP server pre-warms the graph automatically at startup, but the first tool call may need to wait for that to complete. Pre-building ensures instant readiness.

## Step 5: Update AGENTS.md with tool guidance

If AGENTS.md exists in the project root, check if it already contains `<!-- RNA MCP tool guidance -->`. If it already has this marker, skip this step.

If AGENTS.md exists but lacks the marker, append this block:

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
| Searching git history | `oh_search_context(query)` -- returns hash; use `git show <hash>` via Bash for diffs |
<!-- end RNA MCP tool guidance -->
```

If AGENTS.md does not exist, create it with the tool guidance block as the initial content.

## Step 6: Inform the user

Tell the user:
1. Setup is complete
2. They may need to **restart their agent/IDE** for the MCP server to load
3. After restart, RNA MCP tools will be available:
   - `oh_get_context` - Business context in one call
   - `oh_search_context` - Semantic search over outcomes, code, commits
   - `search_symbols` - Multi-language symbol search with graph edges
   - `graph_query` - Impact analysis, neighbor traversal, reachability
   - `outcome_progress` - Structural outcome-to-code joins
   - `list_roots` - Workspace root management
