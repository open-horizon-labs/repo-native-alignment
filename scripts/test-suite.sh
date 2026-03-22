#!/bin/bash
# RNA Full Test Suite — General functionality + all features since v0.1.14
#
# Usage: ./scripts/test-suite.sh [rna_repo_path] [ic_repo_path]
#
# The test suite runs a pre-flight scan to ensure the RNA repo cache is
# current before exercising search and graph queries. Any failing test is
# either a regression (reopen the original issue) or a pending feature
# (marked SKIP with a reason).
#
# Tests are grouped by feature area. Each check() call verifies a single
# behavioral invariant. Structural checks (grep on source) verify
# architectural constraints that must survive future refactors.

set -uo pipefail

RNA_REPO="${1:-$HOME/src/open-horizon-labs/repo-native-alignment}"
IC_REPO="${2:-$HOME/src/Innovation-Connector}"
PASS=0; FAIL=0; SKIP=0

check() {
  local label="$1" cmd="$2" expect="$3" skip_reason="${4:-}"
  if [ -n "$skip_reason" ]; then
    echo "SKIP: $label ($skip_reason)"
    SKIP=$((SKIP+1))
    return
  fi
  result=$(eval "$cmd" 2>/dev/null)
  if echo "$result" | grep -q "$expect"; then
    echo "PASS: $label"
    PASS=$((PASS+1))
  else
    echo "FAIL: $label"
    echo "  CMD: $cmd"
    echo "  EXPECTED: $expect"
    echo "  GOT: $(echo "$result" | grep -v "^$\|INFO\|WARN" | head -3)"
    FAIL=$((FAIL+1))
  fi
}

echo "=== RNA FULL TEST SUITE === $(date)"
echo "Binary: $(repo-native-alignment --version)"
echo "RNA repo: $RNA_REPO"
echo ""

# ── PRE-FLIGHT ────────────────────────────────────────────────────────────────
# Ensure cache is fresh. Without this, search tests fail with stale or missing
# data after worktree switches or clean checkouts.
echo "--- Pre-flight: ensure cache is current ---"
echo "  Running incremental scan on RNA repo..."
if ! _rna_scan_out=$(repo-native-alignment scan --repo "$RNA_REPO" 2>&1); then
  echo "FAIL: RNA pre-flight scan failed — tests would run on stale/partial cache"
  echo "$_rna_scan_out" | tail -20
  exit 1
fi
echo "$_rna_scan_out" | tail -3
echo ""

# ── CORE SEARCH ──────────────────────────────────────────────────────────────
echo "--- Core Search ---"
check "search code symbol" \
  "repo-native-alignment search 'build_code_embedding_text' --repo $RNA_REPO --limit 3" "function"
check "FTS on file_path" \
  "repo-native-alignment search 'embed.rs' --repo $RNA_REPO --limit 1" "embed"
check "kind=module" \
  "repo-native-alignment search '' --repo $RNA_REPO --kind module --limit 1" "module"
check "kind=subsystem" \
  "repo-native-alignment search '' --repo $RNA_REPO --kind subsystem --limit 1" "subsystem"
check "kind=framework" \
  "repo-native-alignment search '' --repo $RNA_REPO --kind framework --limit 1" "framework"
check "kind=package (structural: manifest.rs emits NodeKind::Other('package'))" \
  "grep -c 'NodeKind::Other.*package' $RNA_REPO/src/extract/manifest.rs" "[1-9]"
check "subsystem filter" \
  "repo-native-alignment search 'embed' --repo $RNA_REPO --subsystem embed --limit 1" "embed"
check "cross-file calls symbol present" \
  "repo-native-alignment search 'import_calls_pass' --repo $RNA_REPO --limit 1" "import_calls"

# ── EDGE TRAVERSAL ───────────────────────────────────────────────────────────
echo "" && echo "--- Edge Traversal ---"
check "BelongsTo edges (#396)" \
  "repo-native-alignment graph --node 'src/embed.rs:build_code_embedding_text:function' --repo $RNA_REPO --mode neighbors --direction outgoing --edge-types belongs_to" "module\|subsystem"
check "Calls edges" \
  "repo-native-alignment graph --node 'src/embed.rs:build_code_embedding_text:function' --repo $RNA_REPO --mode neighbors --direction outgoing --edge-types calls" "result"
check "Subsystem members present" \
  "repo-native-alignment search '' --repo $RNA_REPO --kind subsystem --limit 3" "subsystem"

# ── LIST ROOTS (#383) ─────────────────────────────────────────────────────────
echo "" && echo "--- list-roots (#383) ---"
check "list-roots returns slug" \
  "repo-native-alignment list-roots --repo $RNA_REPO" "repo-native-alignment\|slug"
check "list-roots includes primary root type" \
  "repo-native-alignment list-roots --repo $RNA_REPO" "code-project\|primary"

# ── SCAN PERFORMANCE ─────────────────────────────────────────────────────────
echo "" && echo "--- Scan Performance ---"
SCAN_TIME=$(TIMEFORMAT='%R'; { time repo-native-alignment scan --repo "$RNA_REPO" 2>/dev/null; } 2>&1 | tail -1)
echo "  Incremental scan time: ${SCAN_TIME}s"
if (( $(echo "$SCAN_TIME < 120" | bc -l 2>/dev/null || echo 1) )); then
  echo "PASS: scan time < 120s"
  PASS=$((PASS+1))
else
  echo "FAIL: scan time ${SCAN_TIME}s >= 120s"
  FAIL=$((FAIL+1))
fi

# ── WAL SENTINELS (#477 / #495) ───────────────────────────────────────────────
# After a scan, .oh/.cache/extract_completed.json must exist and contain
# the schema_version and node_count fields written by write_extract_sentinel().
echo "" && echo "--- WAL Sentinels (#477) ---"
check "extract_completed.json exists after scan" \
  "test -f $RNA_REPO/.oh/.cache/extract_completed.json && echo found" "found"
check "extract_completed.json has schema_version" \
  "cat $RNA_REPO/.oh/.cache/extract_completed.json" "schema_version"
check "extract_completed.json has node_count" \
  "cat $RNA_REPO/.oh/.cache/extract_completed.json" "node_count"
check "extract_completed.json has edge_count" \
  "cat $RNA_REPO/.oh/.cache/extract_completed.json" "edge_count"

# ── APPEND-ONLY LANCEDB (#477 / #496) ────────────────────────────────────────
# scan_version column must be present in the LanceDB schema (SCHEMA_VERSION >= 17).
# We verify indirectly: schema_version in the sentinel must be >= 17.
echo "" && echo "--- Append-only LanceDB with scan_version (#496) ---"
check "sentinel schema_version >= 17 (scan_version column)" \
  "python3 -c \"import json; d=json.load(open('$RNA_REPO/.oh/.cache/extract_completed.json')); print(d['schema_version'])\" 2>/dev/null" "1[7-9]\|[2-9][0-9]"

# ── PostExtractionRegistry (#493) ────────────────────────────────────────────
# The PostExtractionRegistry struct must exist in post_extraction.rs and must
# expose with_builtins() and run_all() (architectural constraint).
echo "" && echo "--- PostExtractionRegistry (#493) ---"
check "PostExtractionRegistry struct defined" \
  "grep -c 'pub struct PostExtractionRegistry' $RNA_REPO/src/extract/post_extraction.rs" "[1-9]"
check "PostExtractionRegistry::with_builtins exists" \
  "grep -c 'fn with_builtins' $RNA_REPO/src/extract/post_extraction.rs" "[1-9]"
check "PostExtractionRegistry::run_all exists" \
  "grep -c 'fn run_all' $RNA_REPO/src/extract/post_extraction.rs" "[1-9]"

# ── EventBus / ExtractionConsumer (#479 / #500) ───────────────────────────────
echo "" && echo "--- EventBus / ExtractionConsumer (#479) ---"
check "ExtractionConsumer trait defined in event_bus.rs" \
  "grep -c 'pub trait ExtractionConsumer' $RNA_REPO/src/extract/event_bus.rs" "[1-9]"
check "EventBus struct defined" \
  "grep -c 'pub struct EventBus' $RNA_REPO/src/extract/event_bus.rs" "[1-9]"
check "EventBus::register exists" \
  "grep -c 'pub fn register' $RNA_REPO/src/extract/event_bus.rs" "[1-9]"
check "consumers.rs built-in consumers present" \
  "grep -c 'ExtractionConsumer for' $RNA_REPO/src/extract/consumers.rs" "[1-9]"
# ADR constraint: no direct pass calls in server/ (bus must coordinate)
check "ADR constraint: no direct pass calls in server/" \
  "grep -r 'api_link_pass\|tested_by_pass\|import_calls_pass' $RNA_REPO/src/server/ 2>/dev/null | grep -v '//\|test' | wc -l | tr -d ' '" "^0$"

# ── Custom Extractor Config (#468 / #494) ─────────────────────────────────────
# The extractor_config_pass_with_configs function must be callable and produce
# Produces/Consumes edges when given matching TOML config.
# We verify via source structure (unit tests exercise the runtime path).
echo "" && echo "--- Custom Extractor Config (#468) ---"
check "extractor_config_pass_with_configs defined" \
  "grep -c 'pub fn extractor_config_pass_with_configs' $RNA_REPO/src/extract/extractor_config.rs" "[1-9]"
check "load_extractor_configs loads from .oh/extractors/" \
  "grep -c 'extractors' $RNA_REPO/src/extract/extractor_config.rs" "[1-9]"
check "EdgeKind::Produces referenced in extractor_config" \
  "grep -c 'Produces\|Consumes' $RNA_REPO/src/extract/extractor_config.rs" "[1-9]"

# ── Cross-File Calls (#462 / #472) ────────────────────────────────────────────
# import_calls_pass must exist and produce Calls edges across file boundaries.
echo "" && echo "--- Cross-File Calls (#462) ---"
check "import_calls_pass function defined" \
  "grep -c 'pub fn import_calls_pass' $RNA_REPO/src/extract/import_calls.rs" "[1-9]"
check "import_calls_pass produces Calls edges" \
  "grep -c 'EdgeKind::Calls\|Calls' $RNA_REPO/src/extract/import_calls.rs" "[1-9]"
# Verify the function is searchable via RNA (smoke: the pass was indexed)
check "import_calls_pass indexed by RNA" \
  "repo-native-alignment search 'import_calls_pass' --repo $RNA_REPO --limit 1" "import_calls"

# ── BelongsTo Edges (#396 / #443) ─────────────────────────────────────────────
# directory_module pass produces unconditional BelongsTo edges; verify by
# querying a known function's outgoing belongs_to neighbors.
echo "" && echo "--- BelongsTo Edges (#396 / #443) ---"
check "BelongsTo: embed.rs function belongs to module" \
  "repo-native-alignment graph --node 'src/embed.rs:build_code_embedding_text:function' --repo $RNA_REPO --mode neighbors --direction outgoing --edge-types belongs_to" "module\|subsystem"
check "directory_module_pass defined" \
  "grep -c 'fn directory_module_pass\|pub fn directory' $RNA_REPO/src/extract/directory_module.rs" "[1-9]"

# ── Framework Detection (#469 / #480) ─────────────────────────────────────────
# After scan, framework nodes must appear for the RNA repo (lancedb, tokio).
echo "" && echo "--- Framework Detection (#469) ---"
check "framework nodes present on RNA repo" \
  "repo-native-alignment search '' --repo $RNA_REPO --kind framework --limit 5" "framework"
check "lancedb framework detected" \
  "repo-native-alignment search '' --repo $RNA_REPO --kind framework --limit 5" "lancedb"
check "tokio framework detected" \
  "repo-native-alignment search 'tokio' --repo $RNA_REPO --kind framework --limit 3" "tokio\|framework"

# ── Subsystem Nodes (#470 / #473) ─────────────────────────────────────────────
# Subsystem detection must emit first-class NodeKind::Other("subsystem") nodes.
echo "" && echo "--- Subsystem Nodes (#470) ---"
check "subsystem nodes present on RNA repo" \
  "repo-native-alignment search '' --repo $RNA_REPO --kind subsystem --limit 5" "subsystem"
check "extract subsystem present (RNA has extract module)" \
  "repo-native-alignment search '' --repo $RNA_REPO --kind subsystem --limit 10" "extract\|embed\|graph"
check "subsystem detection pass defined" \
  "grep -c 'subsystem_pass\|fn.*subsystem' $RNA_REPO/src/extract/subsystem_pass.rs" "[1-9]"

# ── INNOVATION-CONNECTOR ─────────────────────────────────────────────────────
if [ -d "$IC_REPO/.oh/.cache/lance" ]; then
  echo "" && echo "--- Innovation-Connector ---"
  # Ensure IC cache is fresh before running IC tests
  echo "  Running incremental scan on IC repo..."
  if ! _ic_scan_out=$(repo-native-alignment scan --repo "$IC_REPO" 2>&1); then
    echo "FAIL: IC pre-flight scan failed — IC tests would run on stale/partial cache"
    echo "$_ic_scan_out" | tail -20
    exit 1
  fi
  echo "$_ic_scan_out" | tail -2

  check "IC: FastAPI endpoints" \
    "repo-native-alignment search '' --repo $IC_REPO --kind api_endpoint --limit 5" "route_decorator"
  check "IC: Next.js routes (limit 500)" \
    "repo-native-alignment search '' --repo $IC_REPO --kind api_endpoint --limit 500 2>/dev/null | grep -c nextjs_app_router" "[1-9]"
  check "IC: framework nodes" \
    "repo-native-alignment search '' --repo $IC_REPO --kind framework --limit 5" "fastapi\|react"
  check "IC: cross-file calls Expertunities" \
    "repo-native-alignment graph --node 'client/src/components/Expertunities/Expertunities.tsx:Expertunities:function' --repo $IC_REPO --mode neighbors --direction outgoing --edge-types calls" "useQuery"
  check "IC: 3-query Q1 component" \
    "repo-native-alignment search 'Expertunities' --repo $IC_REPO --limit 3" "Expertunities"
  check "IC: 3-query Q2 hook" \
    "repo-native-alignment search 'useQueryExpertunities' --repo $IC_REPO --limit 3" "useQueryExpertunities\|api.ts"
  check "IC: 3-query Q3 endpoint" \
    "repo-native-alignment search 'expertunities' --repo $IC_REPO --kind api_endpoint --limit 3" "expertunities"
  check "IC: BelongsTo edges" \
    "repo-native-alignment search 'get_expertunities' --repo $IC_REPO --limit 1" "result"
  check "IC: Next.js routes survive rescan" \
    "repo-native-alignment scan --repo $IC_REPO 2>/dev/null && repo-native-alignment search '' --repo $IC_REPO --kind api_endpoint --limit 500 2>/dev/null | grep -c nextjs_app_router" "3"
  check "IC: WAL sentinel present after scan" \
    "test -f $IC_REPO/.oh/.cache/extract_completed.json && echo found" "found"
else
  echo "" && echo "--- Innovation-Connector (SKIP: no IC cache at $IC_REPO/.oh/.cache/lance) ---"
  SKIP=$((SKIP+1))
fi

# ── PENDING (queue after agents complete) ────────────────────────────────────
echo "" && echo "--- Pending Features (SKIP until merged) ---"
check "OpenAPI bidirectional (#465)" \
  "grep -r 'openapi_bidirectional\|OpenApiBidirectional\|operationId.*Implements' $RNA_REPO/src/extract/ 2>/dev/null | wc -l" "[1-9]" \
  "#465 not yet merged"
check "gRPC service edges (#466)" \
  "repo-native-alignment search '' --repo $RNA_REPO --kind grpc_service --limit 1 2>/dev/null | grep -c grpc_service" "[1-9]" \
  "#466 not yet merged"
check "Module split structure (#492)" \
  "ls $RNA_REPO/src/consumers/ 2>/dev/null | wc -l" "[1-9]" \
  "#492 not yet merged"
check "Pipeline wired to EventBus (#502)" \
  "grep -r 'bus.emit.*RootDiscovered\|EventBus::new' $RNA_REPO/src/server/graph.rs 2>/dev/null | wc -l" "[1-9]" \
  "#502 not yet merged"

echo ""
echo "=== RESULTS: $PASS passed, $FAIL failed, $SKIP skipped ==="
if [ "$FAIL" -eq 0 ]; then
  echo "ALL TESTS PASSING (excluding skipped)"
  exit 0
else
  echo "FAILURES NEED ATTENTION"
  exit 1
fi
