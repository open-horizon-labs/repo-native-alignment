#!/bin/bash
set -euo pipefail

BENCH=/Users/muness1/src/open-horizon-labs/repo-native-alignment/benchmark
RUBRIC=$(cat $BENCH/eval-rubric-v3.md)
RESULTS_DIR=$BENCH/results
SCORES_DIR=$BENCH/results/scores
mkdir -p $SCORES_DIR

PARSER=$BENCH/parse-results.py

score_one() {
  local INPUT_FILE=$1
  local SCORE_FILE=$2
  local LABEL=$3

  # Extract the solution text from the transcript
  SOLUTION=$(python3 -c "
import json, sys
with open('$INPUT_FILE') as f:
    solution = ''
    for line in f:
        line = line.strip()
        if not line: continue
        try:
            msg = json.loads(line)
        except: continue
        if msg.get('type') == 'assistant':
            content = msg.get('message', {}).get('content', [])
            if isinstance(content, list):
                for block in content:
                    if isinstance(block, dict) and block.get('type') == 'text':
                        solution = block.get('text', '')
    print(solution)
" 2>/dev/null)

  # If it's a claude --print JSON output, extract differently
  if [ -z "$SOLUTION" ] && [ -f "$INPUT_FILE" ]; then
    SOLUTION=$(python3 -c "
import json
with open('$INPUT_FILE') as f:
    d = json.load(f)
    print(d.get('result', ''))
" 2>/dev/null || true)
  fi

  if [ -z "$SOLUTION" ]; then
    echo "  SKIP $LABEL — no solution found"
    return
  fi

  echo "  Scoring $LABEL..."
  cd /Users/muness1/src/open-horizon-labs/repo-native-alignment
  CLAUDE_CONFIG_DIR=~/.claude-artium claude --print \
    --output-format json \
    --no-session-persistence \
    --max-budget-usd 1 \
    -p "Score this developer exploration report using the rubric below. Output ONLY valid JSON matching the schema in the rubric (the single-solution version: q1 through q5 scores, weighted_score, functions_named, factual_errors, max_hop_depth, notes).

## RUBRIC

$RUBRIC

## SOLUTION TO SCORE

$SOLUTION" \
    > "$SCORE_FILE" 2>/dev/null
  echo "  Done: $SCORE_FILE"
}

echo "=== Scoring all results ==="

# Score vanilla runs
for f in $RESULTS_DIR/vanilla/v3-run-*.jsonl $RESULTS_DIR/vanilla/v3-run-*.json; do
  [ -f "$f" ] || continue
  BASENAME=$(basename "$f" | sed 's/\.\(jsonl\|json\)$//')
  score_one "$f" "$SCORES_DIR/vanilla-${BASENAME}.json" "vanilla/$BASENAME"
done

# Score RNA runs
for f in $RESULTS_DIR/rna/v3-run-*.jsonl $RESULTS_DIR/rna/v3-run-*.json; do
  [ -f "$f" ] || continue
  BASENAME=$(basename "$f" | sed 's/\.\(jsonl\|json\)$//')
  score_one "$f" "$SCORES_DIR/rna-${BASENAME}.json" "rna/$BASENAME"
done

echo "=== Scoring complete ==="
echo "Scores in $SCORES_DIR"
