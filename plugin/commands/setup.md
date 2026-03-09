# Setup Repo-Native Alignment MCP

Install the RNA MCP server for aim-conditioned code intelligence.

**Execute these steps in order:**

## Step 1: Check if already installed

```bash
which repo-native-alignment 2>/dev/null
```

If found, skip to Step 3.

## Step 2: Download the binary

Detect the platform and download the latest release:

```bash
# Detect platform
OS=$(uname -s)
ARCH=$(uname -m)
```

**If macOS ARM** (`Darwin` + `arm64`):
```bash
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-darwin-arm64 -o /usr/local/bin/repo-native-alignment
chmod +x /usr/local/bin/repo-native-alignment
```

**If Linux x86_64** (`Linux` + `x86_64`):
```bash
curl -L https://github.com/open-horizon-labs/repo-native-alignment/releases/latest/download/repo-native-alignment-linux-x86_64 -o /usr/local/bin/repo-native-alignment
chmod +x /usr/local/bin/repo-native-alignment
```

**If neither:** Tell the user their platform is not yet supported. They can build from source:
```bash
cargo install --locked --path . --git https://github.com/open-horizon-labs/repo-native-alignment
```

## Step 3: Add MCP server to Claude Code

RNA is a per-project MCP server (it indexes the repo it's pointed at), so add it with project scope:

```bash
claude mcp add rna-mcp --scope project -- repo-native-alignment --repo .
```

This adds rna-mcp to the project's `.mcp.json` so it activates when working in this repo.

## Step 4: Inform the user

Tell the user:
1. Setup is complete
2. They need to **restart Claude Code** for the MCP to load
3. After restart, RNA MCP tools will be available:
   - `oh_get_context` - Business context in one call
   - `oh_search_context` - Semantic search over outcomes, code, commits
   - `search_symbols` - Multi-language symbol search with graph edges
   - `graph_query` - Impact analysis, neighbor traversal, reachability
   - `outcome_progress` - Structural outcome-to-code joins
   - `oh_record` - Record learnings, signals, guardrails
4. Optional: run `repo-native-alignment setup --project .` for full OH Skills + agents setup
