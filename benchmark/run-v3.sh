#!/bin/bash
set -euo pipefail

BENCH=/Users/muness1/src/open-horizon-labs/repo-native-alignment/benchmark
REPO=/Users/muness1/src/hiphi-repos/unified-hifi-control
VANILLA_PROMPT="$(cat $BENCH/prompt-v3.md)"
RNA_PROMPT="$(cat $BENCH/prompt-v3-rna.md)"

for i in 02 03 04 05; do
  echo "=== Run $i: Vanilla ==="
  cd $REPO
  CLAUDE_CONFIG_DIR=~/.claude-artium claude --print \
    --output-format json \
    --no-session-persistence \
    --max-budget-usd 5 \
    --allowedTools "Grep,Read,Glob,Bash" \
    -p "$VANILLA_PROMPT" \
    > $BENCH/results/vanilla/v3-run-${i}.json 2>/dev/null
  echo "  done"

  echo "=== Run $i: RNA CLI ==="
  cd $REPO
  CLAUDE_CONFIG_DIR=~/.claude-artium claude --print \
    --output-format json \
    --no-session-persistence \
    --max-budget-usd 5 \
    --allowedTools "Grep,Read,Glob,Bash" \
    -p "$RNA_PROMPT" \
    > $BENCH/results/rna/v3-run-${i}.json 2>/dev/null
  echo "  done"
done

echo "=== All runs complete ==="
