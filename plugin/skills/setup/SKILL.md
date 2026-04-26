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

If AGENTS.md does not exist, create it with the tool guidance block as the initial content.

## Step 5b: Check for framework boundary patterns

Check whether the project uses messaging frameworks, event buses, or libraries with hook patterns that RNA should know about. Capture the result so the setup pipeline can branch on it explicitly — do not rely on `grep` exit codes, which return non-zero on no-match and would halt chained automated setup.

```bash
FRAMEWORK_PATTERN='pubsub|kafka|celery|pika|redis|rabbitmq|sqlalchemy|django|flask|opentelemetry|otel'
DETECTED_FRAMEWORKS=$(repo-native-alignment search --repo . "" --kind import --compact 2>/dev/null \
  | grep -iEo "$FRAMEWORK_PATTERN" | sort -u || true)

if [ -z "$DETECTED_FRAMEWORKS" ]; then
  echo "No framework boundaries detected; skipping extractor coverage check."
else
  echo "Detected frameworks: $DETECTED_FRAMEWORKS"
fi
```

If frameworks were detected, assert real coverage in `.oh/extractors/` by searching extractor file **contents** (not just listing filenames). The check below sets `EXTRACTOR_COVERAGE` to the list of frameworks already covered, then computes the gap; both branches exit zero so chained pipelines do not abort.

```bash
if [ -n "$DETECTED_FRAMEWORKS" ]; then
  if [ -d .oh/extractors ]; then
    EXTRACTOR_COVERAGE=$(grep -RhoiE "$FRAMEWORK_PATTERN" .oh/extractors/ 2>/dev/null \
      | sort -u || true)
  else
    # Treat missing dir as zero coverage so the gap output names every detected
    # framework. Do not exit non-zero — chained automated setup must continue.
    EXTRACTOR_COVERAGE=""
  fi
  GAP=$(comm -23 <(echo "$DETECTED_FRAMEWORKS") <(echo "$EXTRACTOR_COVERAGE"))
  if [ -z "$GAP" ]; then
    echo "All detected frameworks have extractor coverage."
  else
    echo "Detected frameworks missing extractor coverage:"
    echo "$GAP"
  fi
fi
```

If the project uses a messaging framework without coverage, create a config file. Example for Google Pub/Sub:

```toml
# .oh/extractors/google-pubsub.toml
[meta]
name = "google-pubsub"
applies_when = { language = "python", imports_contain = "google.cloud.pubsub" }

[[boundaries]]
function_pattern = "publisher.publish"
arg_position = 0
edge_kind = "Produces"

[[boundaries]]
function_pattern = "subscribe"
arg_position = 0
edge_kind = "Consumes"
```

If the project uses frameworks with method-override hooks (SQLAlchemy, Django, OTEL), create a hooks config:

```toml
# .oh/extractors/sqlalchemy-hooks.toml
[meta]
name = "sqlalchemy-hooks"
applies_when = { language = "python", imports_contain = "sqlalchemy" }

[[hooks]]
class_contains = "TypeDecorator"
method_names = ["process_bind_param", "process_result_value"]
```

The `[[boundaries]]` section detects Produces/Consumes edges for message brokers.
The `[[hooks]]` section marks method overrides on library base classes as framework hooks (so dead-code analysis skips them).

For more details, see `docs/extractors.md` in the RNA repo, or use `/gen-extractor` to generate configs from a natural-language description.

## Step 6: Inform the user

Tell the user:
1. Setup is complete
2. They may need to **restart their agent/IDE** for the MCP server to load
3. After restart, RNA MCP tools will be available:
   - `search` - Symbol search, graph traversal (neighbors/impact/reachable/tests_for/cycles/path), artifact/commit/markdown search. Use `compact: true` to save tokens. Use `mode` + `node` for graph walks.
   - `repo_map` - Codebase orientation: top symbols by importance, hotspot files, entry points, subsystem breakdown
   - `outcome_progress` - Business outcome tracking against code changes
   - `list_roots` - Workspace root management
