#!/bin/bash
# Diagnose RNA search recall failures by pipeline stage.
#
# Usage:
#   scripts/search-recall-fixtures.sh [repo]
#
# The script answers "cast wide net, then filter, then rank: which stage failed?"
# for a small fixture set taken from issue #687. It is diagnostic, not a broad
# search benchmark: each fixture has a known source fact and expected retrieval
# text. The output is a markdown table suitable for pasting into the issue.

set -uo pipefail

RNA_REPO="${1:-$(pwd)}"
RNA_BIN="${RNA_BIN:-repo-native-alignment}"
RNA_RECALL_SEMANTIC="${RNA_RECALL_SEMANTIC:-false}"
RNA_RECALL_RERANK="${RNA_RECALL_RERANK:-false}"

TMP_DIR="${TMPDIR:-/tmp}/rna-search-recall-fixtures.$$"
mkdir -p "$TMP_DIR"
trap 'rm -rf "$TMP_DIR"' EXIT

pass_count=0
fail_count=0

have_source() {
  local file="$1" pattern="$2"
  python3 - "$RNA_REPO" "$file" "$pattern" <<'PY'
import re
import sys
from pathlib import Path
root, rel, pattern = sys.argv[1:]
path = Path(root) / rel
if not path.exists():
    sys.exit(1)
text = path.read_text(errors="replace")
sys.exit(0 if re.search(pattern, text, re.MULTILINE | re.DOTALL) else 1)
PY
}

run_search() {
  local query="$1" mode="$2" limit="$3" outfile="$4"
  shift 4
  local status=0
  if "$RNA_BIN" search "$query" \
    --repo "$RNA_REPO" \
    --search-mode "$mode" \
    --limit "$limit" \
    --compact \
    "$@" >"$outfile" 2>&1; then
    return 0
  else
    status=$?
    {
      printf '\nERROR: search probe failed with exit code %s\n' "$status"
      printf 'query=%s mode=%s limit=%s\n' "$query" "$mode" "$limit"
    } >>"$outfile"
    printf 'Search probe failed: query=%s mode=%s exit=%s\n' "$query" "$mode" "$status" >&2
    fail_count=$((fail_count + 1))
    return 0
  fi
}

contains_in_section() {
  local file="$1" section="$2" pattern="$3"
  python3 - "$file" "$section" "$pattern" <<'PY'
import re
import sys
from pathlib import Path
path, section, pattern = sys.argv[1:]
text = Path(path).read_text(errors="replace")
start = text.find(f"### {section}")
if start == -1:
    sys.exit(1)
next_section = text.find("\n### ", start + 1)
chunk = text[start:] if next_section == -1 else text[start:next_section]
sys.exit(0 if re.search(pattern, chunk, re.MULTILINE | re.IGNORECASE | re.DOTALL) else 1)
PY
}

stage_for() {
  local source_ok="$1" default_ok="$2" wide_ok="$3" filtered_ok="$4"
  if [ "$default_ok" = "yes" ]; then
    echo "pass"
  elif [ "$wide_ok" = "yes" ]; then
    echo "ranking/filtering"
  elif [ "$filtered_ok" = "yes" ]; then
    echo "wide-net/query-matching"
  elif [ "$source_ok" = "yes" ]; then
    echo "indexing/candidate-pool"
  else
    echo "corpus/source-gap"
  fi
}

record_fixture() {
  local id="$1" query="$2" section="$3" expected_regex="$4" source_file="$5" source_regex="$6" filtered_query="$7" note="$8"
  shift 8

  local default_out="$TMP_DIR/${id}.default.out"
  local keyword_out="$TMP_DIR/${id}.keyword.out"
  local semantic_out="$TMP_DIR/${id}.semantic.out"
  local filtered_out="$TMP_DIR/${id}.filtered.out"

  local source_ok="no"
  if have_source "$source_file" "$source_regex"; then
    source_ok="yes"
  fi

  # Default approximates MCP hybrid retrieval. Reranking is opt-in here because
  # first-run cross-encoder setup can dominate this diagnostic.
  if [ "$RNA_RECALL_RERANK" = "true" ]; then
    run_search "$query" hybrid 10 "$default_out" --rerank "$@"
  else
    run_search "$query" hybrid 10 "$default_out" "$@"
  fi
  # Wide net is mode-specific. If neither keyword nor semantic sees the fact,
  # rank/fusion cannot recover it. Semantic probing is opt-in because it can
  # dominate wall-clock time when the benchmark's immediate target is exact/code recall.
  run_search "$query" keyword 100 "$keyword_out" "$@"
  if [ "$RNA_RECALL_SEMANTIC" = "true" ]; then
    run_search "$query" semantic 100 "$semantic_out" "$@"
  else
    printf 'semantic probe skipped; set RNA_RECALL_SEMANTIC=true to enable\n' >"$semantic_out"
  fi
  # Filtered/control: exact atom or narrower query. If this succeeds while wide
  # fails, the corpus has a fact but query matching is too brittle.
  run_search "$filtered_query" keyword 100 "$filtered_out" "$@"

  local default_ok="no" keyword_ok="no" semantic_ok="no" filtered_ok="no"
  if contains_in_section "$default_out" "$section" "$expected_regex"; then default_ok="yes"; fi
  if contains_in_section "$keyword_out" "$section" "$expected_regex"; then keyword_ok="yes"; fi
  if [ "$RNA_RECALL_SEMANTIC" = "true" ] && contains_in_section "$semantic_out" "$section" "$expected_regex"; then semantic_ok="yes"; elif [ "$RNA_RECALL_SEMANTIC" != "true" ]; then semantic_ok="skipped"; fi
  if contains_in_section "$filtered_out" "$section" "$expected_regex"; then filtered_ok="yes"; fi

  local stage wide_ok
  if [ "$keyword_ok" = "yes" ] || [ "$semantic_ok" = "yes" ]; then
    wide_ok="yes"
  else
    wide_ok="no"
  fi
  stage=$(stage_for "$source_ok" "$default_ok" "$wide_ok" "$filtered_ok")

  printf '| `%s` | `%s` | %s | %s | %s | %s | %s | %s | %s | %s |\n' \
    "$id" "$query" "$section" "$source_ok" "$default_ok" "$keyword_ok" "$semantic_ok" "$filtered_ok" "$stage" "$note"

  pass_count=$((pass_count + 1))
}

record_corpus_gap() {
  local id="$1" query="$2" source_file="$3" source_regex="$4" note="$5"
  local source_ok="no"
  if have_source "$source_file" "$source_regex"; then
    source_ok="yes"
  fi
  local stage="corpus/source-gap"
  if [ "$source_ok" = "yes" ]; then
    stage="needs-search-probe"
  fi
  printf '| `%s` | `%s` | n/a | %s | n/a | n/a | n/a | n/a | %s | %s |\n' \
    "$id" "$query" "$source_ok" "$stage" "$note"
  pass_count=$((pass_count + 1))
}

if ! command -v "$RNA_BIN" >/dev/null 2>&1; then
  echo "RNA binary not found: $RNA_BIN" >&2
  exit 127
fi

if [ ! -d "$RNA_REPO" ]; then
  echo "Repo path not found: $RNA_REPO" >&2
  exit 2
fi

cat <<'MD'
# Search recall fixture diagnosis

| Fixture | Query | Expected section | Source fact exists | Default top-10 | Keyword top-100 | Semantic top-100 | Filtered/control | Stage | Note |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
MD

record_fixture \
  "exact-parser-package" \
  "tree-sitter-rust" \
  "Code symbols" \
  "tree-sitter-rust|Cargo\.toml" \
  "Cargo.toml" \
  "tree-sitter-rust" \
  "tree-sitter-rust" \
  "Exact dependency lookup should be a regression guard."

record_fixture \
  "multi-parser-packages" \
  "tree-sitter-rust tree-sitter-python tree-sitter-typescript tree-sitter-javascript tree-sitter-go" \
  "Code symbols" \
  "tree-sitter-rust|tree-sitter-python|tree-sitter-typescript|tree-sitter-javascript|tree-sitter-go" \
  "Cargo.toml" \
  "tree-sitter-rust.*tree-sitter-python.*tree-sitter-typescript|tree-sitter-python.*tree-sitter-rust" \
  "tree-sitter-rust" \
  "If wide succeeds but default misses, tune ranking/fusion only after candidate-pool proof."

record_fixture \
  "langconfig-compound" \
  "LangConfig language parser tree_sitter extractor suffixes" \
  "Code symbols" \
  "LangConfig|configs\.rs|generic\.rs" \
  "src/extract/generic.rs" \
  "struct LangConfig" \
  "LangConfig" \
  "Compound capability-style query should retrieve config facts." \
  --file src/extract --language rust

record_fixture \
  "parser-registration-expression" \
  "tree_sitter_python::LANGUAGE" \
  "Code symbols" \
  "tree_sitter_python|LANGUAGE|configs\.rs|python\.rs" \
  "src/extract/configs.rs" \
  "tree_sitter_python::LANGUAGE" \
  "tree_sitter_python" \
  "Expression-level parser registration or usage must enter the candidate pool before ranking can help." \
  --file src/extract --language rust

record_fixture \
  "lsp-vs-treesitter-doc" \
  "what does LSP add beyond tree-sitter" \
  "Markdown" \
  "What LSP Adds Beyond tree-sitter|docs/lsp-enrichment\.md|LSP" \
  "docs/lsp-enrichment.md" \
  "What LSP Adds Beyond tree-sitter" \
  "What LSP Adds Beyond tree-sitter" \
  "Doc fact exists; classify whether default ranking surfaces it."

record_fixture \
  "manual-parser-policy" \
  "do we need to manually add parsers" \
  "Markdown" \
  "Tree-sitter parser support policy|upstream tree-sitter parser list|candidate catalog|packaged" \
  "docs/extractors.md" \
  "upstream tree-sitter parser list.*candidate catalog|packaged.*registered.*configured.*tested" \
  "Tree-sitter parser support policy" \
  "Policy fact should be discoverable through docs once the local source exists."

cat <<MD

Summary: ${pass_count} fixture(s) evaluated, ${fail_count} diagnostic error(s).
MD

exit "$fail_count"
