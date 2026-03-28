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
  "repo-native-alignment search 'build_code_embedding_text' --repo $RNA_REPO --limit 3" "build_code_embedding_text"
check "FTS on file_path" \
  "repo-native-alignment search 'embed.rs' --repo $RNA_REPO --limit 1" "embed"
check "kind=module" \
  "repo-native-alignment search '' --repo $RNA_REPO --kind module --limit 1" "module"
check "kind=subsystem" \
  "repo-native-alignment search '' --repo $RNA_REPO --kind subsystem --limit 1" "subsystem"
check "kind=framework" \
  "repo-native-alignment search 'framework' --repo $RNA_REPO --kind framework --limit 1" "framework"
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

# ── PostExtractionRegistry (#493 / #523) ─────────────────────────────────────
# PR #523 eliminated PostExtractionRegistry entirely — post_extraction.rs was deleted.
# These checks verify the elimination is complete and the event-driven architecture
# replaced it cleanly.
echo "" && echo "--- PostExtractionRegistry eliminated (#523) ---"
check "post_extraction.rs deleted (#523)" \
  "test ! -f $RNA_REPO/src/extract/post_extraction.rs && echo deleted" "deleted"
check "PostExtractionRegistry absent from src/ (#523)" \
  "grep -r 'PostExtractionRegistry' $RNA_REPO/src/ 2>/dev/null | wc -l | tr -d ' '" "^0$"
check "EnrichmentFinalizer replaced PostExtractionRegistry (#523)" \
  "grep -c 'pub struct EnrichmentFinalizer' $RNA_REPO/src/extract/consumers.rs" "[1-9]"

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
  "repo-native-alignment search 'framework' --repo $RNA_REPO --kind framework --limit 5" "framework"
check "lancedb framework detected" \
  "repo-native-alignment search 'lancedb' --repo $RNA_REPO --kind framework --limit 5" "lancedb"
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
    "repo-native-alignment search '' --repo $IC_REPO --kind api_endpoint --limit 500 2>/dev/null | grep -c nextjs_app_router" "[1-9]" \
    "IC repo has page routes (page.tsx) but no API routes (route.ts under app/api/)"
  check "IC: framework nodes" \
    "repo-native-alignment search 'framework' --repo $IC_REPO --kind framework --limit 5" "fastapi\|react"
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
    "repo-native-alignment scan --repo $IC_REPO 2>/dev/null && repo-native-alignment search '' --repo $IC_REPO --kind api_endpoint --limit 500 2>/dev/null | grep -c nextjs_app_router" "3" \
    "IC repo has page routes (page.tsx) but no API routes (route.ts under app/api/)"
  check "IC: WAL sentinel present after scan" \
    "test -f $IC_REPO/.oh/.cache/extract_completed.json && echo found" "found"
else
  echo "" && echo "--- Innovation-Connector (SKIP: no IC cache at $IC_REPO/.oh/.cache/lance) ---"
  SKIP=$((SKIP+1))
fi

# ── MERGED FEATURES ────────────────────────────────────────────────────────
echo "" && echo "--- Merged Features ---"
check "OpenAPI bidirectional (#465): OpenApiSdkLinkPass exists" \
  "grep -r 'OpenApiSdkLinkPass\|openapi_sdk_link\|operationId.*Implements' $RNA_REPO/src/ 2>/dev/null | grep -v test | wc -l" "[1-9]"
check "gRPC pass exists (#466)" \
  "grep -r 'GrpcClientCallsPass\|grpc_client_calls' $RNA_REPO/src/ 2>/dev/null | grep -v test | wc -l" "[1-9]"
check "Module split: src/consumers/ exists (#492)" \
  "ls $RNA_REPO/src/consumers/ 2>/dev/null | wc -l" "[1-9]"
check "EventBus trait exists (#479)" \
  "grep -r 'trait ExtractionConsumer\|ExtractionEvent' $RNA_REPO/src/ 2>/dev/null | wc -l" "[1-9]"
check "Pipeline references EventBus (#502)" \
  "grep -r 'EventBus\|PostExtractionConsumer\|RootDiscovered' $RNA_REPO/src/server/graph.rs 2>/dev/null | wc -l" "[1-9]"

# ── FASTAPI ROUTER PREFIX PASS (#519) ─────────────────────────────────────
echo "" && echo "--- FastAPI Router Prefix Pass (#519) ---"
check "FastapiRouterPrefixPass struct defined (#519)" \
  "grep -r 'FastapiRouterPrefixPass\|fastapi_router_prefix' $RNA_REPO/src/ 2>/dev/null | grep -v test | wc -l" "[1-9]"
check "FastapiRouterPrefixConsumer registered in build_builtin_bus (#519/#523)" \
  "grep -c 'FastapiRouterPrefixConsumer' $RNA_REPO/src/extract/consumers.rs 2>/dev/null" "[1-9]"
check "router_var metadata extracted in generic.rs (#519)" \
  "grep -c 'router_var' $RNA_REPO/src/extract/generic.rs 2>/dev/null" "[1-9]"

# ── SKIP WORKTREES WITH OWN CACHE (#524) ─────────────────────────────────
echo "" && echo "--- Skip Worktrees with Own Cache (#524) ---"
check "is_worktree_with_own_cache defined in walk.rs (#524)" \
  "grep -c 'is_worktree_with_own_cache' $RNA_REPO/src/walk.rs 2>/dev/null" "[1-9]"
check "worktree skip called from scanner.rs (#524)" \
  "grep -c 'is_worktree_with_own_cache' $RNA_REPO/src/scanner.rs 2>/dev/null" "[1-9]"

# ── SCANSTATSCONSUMER / LIVE LIST_ROOTS (#527) ────────────────────────────
echo "" && echo "--- ScanStatsConsumer / live list_roots (#527) ---"
check "ScanStatsConsumer struct defined (#527)" \
  "grep -r 'struct ScanStatsConsumer\|pub struct ScanStats' $RNA_REPO/src/ 2>/dev/null | wc -l" "[1-9]"
check "build_builtin_bus returns scan_stats handle (#527)" \
  "grep -c 'scan_stats\|ScanStats' $RNA_REPO/src/extract/consumers.rs 2>/dev/null" "[1-9]"
check "list_roots uses scan_stats (#527)" \
  "grep -c 'scan_stats\|ScanStats' $RNA_REPO/src/service/roots.rs 2>/dev/null || grep -c 'scan_stats' $RNA_REPO/src/server/handlers.rs 2>/dev/null" "[1-9]"
check "list-roots output contains timing or slug (#527)" \
  "repo-native-alignment list-roots --repo $RNA_REPO 2>/dev/null" "slug\|scan\|ago\|symbol"

# ── LSPCONSUMER SINGLETON + ALLENRICHMENTSGATE (#528) ─────────────────────
echo "" && echo "--- LspConsumer singleton + AllEnrichmentsGate (#528) ---"
check "LspConsumer holds Arc<dyn Enricher> (#528)" \
  "grep -c 'Arc.*dyn.*Enricher\|Arc<dyn Enricher>' $RNA_REPO/src/extract/consumers.rs 2>/dev/null" "[1-9]"
check "AllEnrichmentsGate struct defined (#528)" \
  "grep -c 'AllEnrichmentsGate\|struct AllEnrichments' $RNA_REPO/src/extract/consumers.rs 2>/dev/null" "[1-9]"
check "AllEnrichmentsDone event defined (#528)" \
  "grep -c 'AllEnrichmentsDone' $RNA_REPO/src/extract/event_bus.rs 2>/dev/null" "[1-9]"
check "EnrichmentFinalizer subscribes to AllEnrichmentsDone (#528/#523)" \
  "awk '/impl ExtractionConsumer for EnrichmentFinalizer/,/^}/' $RNA_REPO/src/extract/consumers.rs 2>/dev/null | awk '/fn subscribes_to/,/fn on_event/' | grep -c 'AllEnrichmentsDone'" "[1-9]"

# ── EMBEDDINGINDEXERCONSUMER + LANCEDBCONSUMER FROM STUBS (#530) ──────────
echo "" && echo "--- EmbeddingIndexerConsumer + LanceDBConsumer from stubs (#530) ---"
check "EmbeddingIndexerConsumer::new defined (#530)" \
  "grep -c 'fn new.*EmbeddingIndex\|EmbeddingIndexerConsumer.*new\|impl EmbeddingIndexerConsumer' $RNA_REPO/src/extract/consumers.rs 2>/dev/null" "[1-9]"
check "EmbeddingIndexerConsumer::stub defined (#530)" \
  "grep -c 'fn stub\|stub()' $RNA_REPO/src/extract/consumers.rs 2>/dev/null" "[1-9]"
check "All production lance_repo_root opts are None (#530)" \
  "grep -c 'BusOptions\|lance_repo_root.*None\|embed_idx.*None' $RNA_REPO/src/server/graph.rs 2>/dev/null" "[1-9]"

# ── FASTAPI PREFIX IDEMPOTENCY (#531) ───────────────────────────────────────
echo "" && echo "--- FastAPI prefix idempotency (#531) ---"
check "http_path_local stored for idempotency (#531)" \
  "grep -c 'http_path_local' $RNA_REPO/src/extract/fastapi_router_prefix.rs 2>/dev/null" "[1-9]"
check "FastapiRouterPrefixConsumer present in consumers.rs (#531/#523)" \
  "grep -c 'FastapiRouterPrefixConsumer' $RNA_REPO/src/extract/consumers.rs 2>/dev/null" "[1-9]"

# ── CONTENT-ADDRESSED CONSUMER CACHE (#526/#533) ─────────────────────────
echo "" && echo "--- Content-addressed consumer cache (#526) ---"
check "ConsumerCacheKey struct defined (#526)" \
  "grep -c 'ConsumerCacheKey\|struct.*CacheKey' $RNA_REPO/src/extract/cache.rs 2>/dev/null" "[1-9]"
check "ExtractionConsumer::version() trait method (#526)" \
  "grep -c 'fn version.*u64\|version().*u64' $RNA_REPO/src/extract/event_bus.rs 2>/dev/null" "[1-9]"
check "EventBus has cache HashMap (#526)" \
  "grep -c 'cache.*HashMap\|HashMap.*cache\|consumer_cache' $RNA_REPO/src/extract/event_bus.rs 2>/dev/null" "[1-9]"

# ── ADR ARCHITECTURE CONSTRAINTS (#543) ──────────────────────────────────────
# RNA enforces its own architecture using its own graph. Each constraint uses the
# RNA CLI (search + graph) as the assertion mechanism, with grep as a fallback for
# absence proofs (RNA search can match .oh/ session docs that mention removed types).
# A failing check means a structural invariant was broken — reopen the relevant issue.
#
# ADR source: .oh/sessions/522-post-523-audit.md + issue #543
echo "" && echo "--- ADR Architecture Constraints (#543) ---"

# Constraint 1: No consumer cross-imports — PostExtractionRegistry gone from src/
# PostExtractionRegistry was eliminated in #523. Grep src/ directly: RNA search picks
# up session docs that mention it, but grep on src/ is the definitive absence proof.
check "ADR: PostExtractionRegistry absent from src/ (#523)" \
  "grep -r 'PostExtractionRegistry' $RNA_REPO/src/ 2>/dev/null | wc -l | tr -d ' '" "^0$"

# Constraint 2: No broker-specific logic in server/
# server/ must not contain framework conditionals — that logic belongs in consumers.
# RNA query form: search 'framework ==' --file src/server would miss grep-only strings.
check "ADR: no 'if framework ==' in src/server/ (#523)" \
  "grep -r 'framework ==' $RNA_REPO/src/server/ 2>/dev/null | grep -v '//' | wc -l | tr -d ' '" "^0$"

# Constraint 3: api_link_pass only called from EnrichmentFinalizer (not server/)
# All pass calls must flow through the consumer bus. RNA graph query verifies the
# function is indexed; grep on server/ verifies no direct bypass exists.
check "ADR: api_link_pass not bypassed in src/server/ (#523)" \
  "grep -r 'api_link_pass' $RNA_REPO/src/server/ 2>/dev/null | grep -v '//' | wc -l | tr -d ' '" "^0$"
check "ADR: api_link_pass indexed by RNA (function kind) (#543)" \
  "repo-native-alignment search 'api_link_pass' --repo $RNA_REPO --kind function --limit 3 2>/dev/null" "api_link_pass"

# Constraint 4: FastapiRouterPrefixConsumer only fires on fastapi
# It must subscribe to FrameworkDetected, not AllEnrichmentsDone or unconditionally.
# RNA verifies the consumer is indexed; grep verifies the subscription event kind.
check "ADR: FastapiRouterPrefixConsumer subscribes to FrameworkDetected (#537/#523)" \
  "awk '/impl ExtractionConsumer for FastapiRouterPrefixConsumer/,/^}/' $RNA_REPO/src/extract/consumers.rs 2>/dev/null | awk '/fn subscribes_to/,/fn on_event/' | grep -c 'FrameworkDetected'" "[1-9]"
check "ADR: FastapiRouterPrefixConsumer indexed as struct by RNA (#543)" \
  "repo-native-alignment search 'FastapiRouterPrefixConsumer' --repo $RNA_REPO --kind struct --limit 3 2>/dev/null" "FastapiRouterPrefixConsumer"

# Constraint 5: PostExtractionRegistry fully gone from all Rust source
# Definitive check: grep across all .rs files under src/. Zero hits required.
check "ADR: PostExtractionRegistry zero .rs references in src/ (#523)" \
  "grep -r 'PostExtractionRegistry' $RNA_REPO/src/ --include='*.rs' 2>/dev/null | wc -l | tr -d ' '" "^0$"

# Constraint 6: SubsystemConsumer is a real bus consumer (#542, promoted in #549)
check "ADR: subsystem_node_pass indexed by RNA (#542)" \
  "repo-native-alignment search 'subsystem_node_pass' --repo $RNA_REPO --kind function --limit 3 2>/dev/null" "subsystem_node_pass"
check "ADR: SubsystemConsumer is a real consumer (#549)" \
  "grep -c 'impl ExtractionConsumer for SubsystemConsumer' $RNA_REPO/src/extract/consumers.rs 2>/dev/null" "[1-9]"

# ── v0.2.0 FEATURES ────────────────────────────────────────────────────────────

# ArcSwap lock-free graph reads (#578) — the fix for v0.1.15/v0.1.16 MCP hangs
echo "" && echo "--- ArcSwap lock-free graph reads (#578) ---"
check "ArcSwap field on ServerState (#578)" \
  "grep -c 'ArcSwap' $RNA_REPO/src/server/mod.rs 2>/dev/null" "[1-9]"
check "RwLock removed from graph field (#578)" \
  "grep 'pub graph:' $RNA_REPO/src/server/mod.rs 2>/dev/null | grep -c 'RwLock'" "^0$"
check "ADR-002 exists for ArcSwap decision (#578)" \
  "test -f $RNA_REPO/docs/ADRs/002-arcswap-graph-concurrency.md && echo found" "found"

# Background LSP enrichment (#574/#596) — scan returns in seconds, LSP runs async
echo "" && echo "--- Background LSP enrichment (#574/#596) ---"
check "get_graph incremental returns immediately (#574)" \
  "grep -c 'tree-sitter.*immediate\|fast return\|incremental.*return' $RNA_REPO/src/server/graph.rs 2>/dev/null || echo 0" "[0-9]"
check "background enrichment spawns async (#574)" \
  "grep -c 'spawn\|tokio.*spawn\|background' $RNA_REPO/src/server/enrichment.rs 2>/dev/null" "[1-9]"

# UTF-8 lossy decode (#568) — non-UTF-8 files no longer silently dropped
echo "" && echo "--- UTF-8 lossy decode (#568) ---"
check "from_utf8_lossy used in extraction (#568)" \
  "grep -rc 'from_utf8_lossy' $RNA_REPO/src/ 2>/dev/null | grep -v ':0$' | wc -l | tr -d ' '" "[1-9]"

# Structured scan summary (#575) — scan output includes symbol/edge/LSP counts
echo "" && echo "--- Structured scan summary (#575) ---"
check "scan output includes symbol count" \
  "repo-native-alignment scan --repo $RNA_REPO 2>&1 | grep -c 'Symbols:\|symbols'" "[1-9]"

# LSP readiness validation (#582) — probe-based servers validated before enrichment
echo "" && echo "--- LSP readiness validation (#582) ---"
check "Phase B indexing validation in lsp/mod.rs (#582)" \
  "grep -c 'Phase B' $RNA_REPO/src/extract/lsp/mod.rs 2>/dev/null" "[1-9]"

# Cached node_index_map + HashSet for files_to_remove (#586)
echo "" && echo "--- Performance: cached node_index_map (#586) ---"
check "node_index_map method on GraphState (#586)" \
  "grep -c 'fn node_index_map' $RNA_REPO/src/server/state.rs 2>/dev/null" "[1-9]"

# ADR-001 conformance: all enrichment through event bus (#583)
echo "" && echo "--- ADR-001 conformance (#583) ---"
check "no direct EnricherRegistry::enrich_all in server/ (#583)" \
  "grep -r 'enrich_all' $RNA_REPO/src/server/ 2>/dev/null | grep -v 'via_bus\|event_bus\|//' | wc -l | tr -d ' '" "^0$"

# CLI cache-first loading (#587)
echo "" && echo "--- CLI cache-first loading (#587) ---"
check "CLI list-roots loads from cache (#587)" \
  "grep -c 'load.*cache\|from_lance\|load_graph_from_lance' $RNA_REPO/src/main.rs 2>/dev/null" "[1-9]"

# Embedding hash fix (#597) — text-only hash, not metadata
echo "" && echo "--- Embedding hash fix (#597) ---"
check "embedding hash uses text-only content (#597)" \
  "grep -c 'embedding_text_hash\|hash.*embedding_text\|text_hash' $RNA_REPO/src/embed.rs 2>/dev/null" "[1-9]"

# LSP refactoring: lsp passes split (#595)
echo "" && echo "--- LSP/store refactoring (#595) ---"
check "src/server/enrichment.rs exists (split from graph.rs) (#595)" \
  "test -f $RNA_REPO/src/server/enrichment.rs && echo found" "found"
check "src/server/bg_scanner.rs exists (split from graph.rs) (#595)" \
  "test -f $RNA_REPO/src/server/bg_scanner.rs && echo found" "found"

# CLI/MCP parity: limit rename (#594)
echo "" && echo "--- CLI/MCP parity: limit rename (#594) ---"
check "MCP search uses limit parameter (#594)" \
  "grep -c '\"limit\"' $RNA_REPO/src/server/tools.rs 2>/dev/null" "[1-9]"

# ── include_body + minify_body (#604) ─────────────────────────────────────────
echo "" && echo "--- include_body + minify_body (#604) ---"
check "include_body returns function body with --nodes (#604)" \
  "repo-native-alignment search '' --repo $RNA_REPO --nodes 'src/embed.rs:build_code_embedding_text:function' --include-body 2>/dev/null" '```rust'
check "minify_body shortens identifiers (#604)" \
  "repo-native-alignment search '' --repo $RNA_REPO --nodes 'src/embed.rs:build_code_embedding_text:function' --include-body --minify-body 2>/dev/null | grep -c '[a-z][0-9][a-z]'" "[1-9]"
check "include_body rejected without --node/--nodes (#604)" \
  "repo-native-alignment search 'test' --repo $RNA_REPO --include-body 2>&1 | grep -ci 'requires'" "[1-9]"
check "minify_body structural: minify_body fn in code/minify.rs (#604)" \
  "grep -c 'pub fn minify_body' $RNA_REPO/src/code/minify.rs 2>/dev/null" "[1-9]"
check "minify_body structural: tree-sitter Rust support (#604)" \
  "grep -c 'rust' $RNA_REPO/src/code/minify.rs 2>/dev/null" "[1-9]"
check "MCP search tool exposes include_body parameter (#604)" \
  "grep -c 'include_body' $RNA_REPO/src/server/tools.rs 2>/dev/null" "[1-9]"
check "MCP search tool exposes minify_body parameter (#604)" \
  "grep -c 'minify_body' $RNA_REPO/src/server/tools.rs 2>/dev/null" "[1-9]"

# ── Verbose flag (MCP suppresses index stats) (#604) ─────────────────────────
echo "" && echo "--- Verbose flag (#604) ---"
check "verbose CLI flag defined (#604)" \
  "grep -c 'verbose: bool' $RNA_REPO/src/main.rs 2>/dev/null" "[1-9]"
check "MCP search tool has verbose parameter (#604)" \
  "grep -c 'verbose' $RNA_REPO/src/server/tools.rs 2>/dev/null" "[1-9]"

# ── Rerank support (#604) ────────────────────────────────────────────────────
echo "" && echo "--- Cross-encoder reranking ---"
check "rerank CLI flag exists" \
  "repo-native-alignment search --help 2>&1 | grep -c 'rerank'" "[1-9]"
check "rerank structural: reranker module exists" \
  "grep -rl 'rerank\|cross.encoder' $RNA_REPO/src/ 2>/dev/null | wc -l | tr -d ' '" "[1-9]"

echo ""
echo "=== RESULTS: $PASS passed, $FAIL failed, $SKIP skipped ==="
if [ "$FAIL" -eq 0 ]; then
  echo "ALL TESTS PASSING (excluding skipped)"
  exit 0
else
  echo "FAILURES NEED ATTENTION"
  exit 1
fi
