#!/bin/bash
# RNA vs Vanilla Benchmark Runner
set -euo pipefail

REPO="/Users/muness1/src/hiphi-repos/unified-hifi-control"
RESULTS_DIR="/Users/muness1/src/open-horizon-labs/repo-native-alignment/benchmark/results"
PROMPT_FILE="/Users/muness1/src/open-horizon-labs/repo-native-alignment/benchmark/prompt-v2.md"
RNA_PROMPT_FILE="/Users/muness1/src/open-horizon-labs/repo-native-alignment/benchmark/prompt-v2-rna.md"
RUNS=${1:-1}

mkdir -p "$RESULTS_DIR/vanilla" "$RESULTS_DIR/rna"

echo "=== RNA vs Vanilla Benchmark ==="
echo "Repo: $REPO"
echo "Runs: $RUNS"

for i in $(seq -w 1 "$RUNS"); do
  echo "--- Run $i ---"

  echo "  Vanilla..."
  cd "$REPO"
  CLAUDE_CONFIG_DIR=~/.claude-artium claude --print \
    --output-format json \
    --no-session-persistence \
    --max-budget-usd 5 \
    --allowedTools "Grep,Read,Glob,Bash" \
    -p "$(cat "$PROMPT_FILE")" \
    > "$RESULTS_DIR/vanilla/run-${i}.json" 2>/dev/null
  echo "  Vanilla done"

  echo "  RNA MCP..."
  cd "$REPO"
  CLAUDE_CONFIG_DIR=~/.claude-artium claude --print \
    --output-format json \
    --no-session-persistence \
    --max-budget-usd 5 \
    --allowedTools "mcp__rna-server__search,mcp__rna-server__repo_map,mcp__rna-server__list_roots,mcp__rna-server__outcome_progress,Grep,Read,Glob,Bash" \
    -p "$(cat "$RNA_PROMPT_FILE")" \
    > "$RESULTS_DIR/rna/run-${i}.json" 2>/dev/null
  echo "  RNA done"

  echo ""
done

echo "=== Complete. Results in $RESULTS_DIR ==="
