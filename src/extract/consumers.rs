//! Built-in `ExtractionConsumer` implementations.
//!
//! Each consumer wraps an existing extraction pass and subscribes to the
//! appropriate event. No consumer imports or calls another consumer.
//!
//! # Registration order
//!
//! Consumers must be registered in the order shown in `build_builtin_bus()`.
//! The ordering invariant:
//!
//! 1. `ManifestConsumer` — subscribes to `RootDiscovered` (needs filesystem)
//! 2. `TreeSitterConsumer` — subscribes to `RootDiscovered`, emits `RootExtracted`
//! 3. `LanguageAccumulatorConsumer` — subscribes to `RootExtracted`, emits `LanguageDetected`
//! 4. `LspConsumer(lang)` × N — subscribes to `LanguageDetected`, runs real LSP enrichment,
//!    emits `EnrichmentComplete` with actual edges and virtual nodes
//! 5. `AllEnrichmentsGate` — subscribes to both `RootExtracted` (to count expected languages)
//!    and `EnrichmentComplete` (to count completions); when counts match emits `AllEnrichmentsDone`
//! 6. `ApiLinkConsumer`, `TestedByConsumer` — `AllEnrichmentsDone` → `PassComplete` (monitoring)
//!    `EnrichmentFinalizer` — `AllEnrichmentsDone` → `PassesComplete` + `FrameworkDetected`
//! 7. `EmbeddingIndexerConsumer` — subscribes to `RootExtracted` (streaming embed as nodes arrive)
//!
//! The persistence consumers (`SubsystemConsumer`, `LanceDBConsumer`) subscribe to `PassesComplete`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;

use crate::extract::ExtractorRegistry;
use crate::extract::event_bus::{ExtractionConsumer, ExtractionEvent, ExtractionEventKind};
use crate::extract::scan_stats::{ScanStats, ScanStatsConsumer};
use crate::graph::Node;

// ---------------------------------------------------------------------------
// ManifestConsumer
// ---------------------------------------------------------------------------

/// Runs `manifest_pass` when a root is discovered.
///
/// Subscribes to: `RootDiscovered`
/// Emits: nothing (manifest nodes are generated inside `EnrichmentFinalizer`
/// which has access to `root_pairs` and the full node set).
///
/// In the ADR's design, `manifest_pass` subscribes to `RootDiscovered` because
/// it needs filesystem access (reads `package.json`, `Cargo.toml`, etc.).
/// For Phase 2/3, manifest runs inside `EnrichmentFinalizer` (which runs on
/// `AllEnrichmentsDone`) to avoid duplicating filesystem reads. This consumer
/// establishes the subscription slot for Phase 4+ when manifest is promoted.
pub struct ManifestConsumer;

#[async_trait]
impl ExtractionConsumer for ManifestConsumer {
    fn name(&self) -> &str { "manifest" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootDiscovered]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::RootDiscovered { slug, .. } = event else {
            return Ok(vec![]);
        };
        tracing::debug!("ManifestConsumer: root '{}' discovered (manifest handled by EnrichmentFinalizer)", slug);
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// TreeSitterConsumer
// ---------------------------------------------------------------------------

/// Runs tree-sitter extraction when a root is discovered.
///
/// Subscribes to: `RootDiscovered`
/// Emits: `RootExtracted` (carrying all nodes + edges for this root)
pub struct TreeSitterConsumer {
    registry: ExtractorRegistry,
    /// Optional handle to live scan stats — used to propagate encoding stats
    /// (binary skipped / lossy-decoded counts) without changing the event schema.
    scan_stats: Option<Arc<RwLock<ScanStats>>>,
}

impl TreeSitterConsumer {
    pub fn new() -> Self {
        Self {
            registry: ExtractorRegistry::with_builtins(),
            scan_stats: None,
        }
    }

    /// Create a consumer that also records encoding stats to shared `ScanStats`.
    pub fn with_scan_stats(scan_stats: Arc<RwLock<ScanStats>>) -> Self {
        Self {
            registry: ExtractorRegistry::with_builtins(),
            scan_stats: Some(scan_stats),
        }
    }
}

impl Default for TreeSitterConsumer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExtractionConsumer for TreeSitterConsumer {
    fn name(&self) -> &str { "tree_sitter" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootDiscovered]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::RootDiscovered { slug, path, lsp_only } = event else {
            return Ok(vec![]);
        };

        // lsp_only roots have no files to extract (their files are covered by the primary root).
        if *lsp_only {
            tracing::debug!("TreeSitterConsumer: skipping lsp_only root '{}'", slug);
            return Ok(vec![]);
        }

        // Build a ScanResult that includes all files in the root.
        let mut scanner = crate::scanner::Scanner::new(path.clone())?;
        let all_files = {
            let _ = scanner.scan(); // populate internal state
            scanner.all_known_files()
        };

        let full_scan = crate::scanner::ScanResult {
            changed_files: Vec::new(),
            new_files: all_files,
            deleted_files: Vec::new(),
            scan_duration: std::time::Duration::ZERO,
        };

        let (mut extraction, enc_stats) = self.registry.extract_scan_result_with_stats(path, &full_scan);

        // Stamp all nodes/edges with the root slug.
        for node in &mut extraction.nodes {
            node.id.root = slug.clone();
        }
        for edge in &mut extraction.edges {
            edge.from.root = slug.clone();
            edge.to.root = slug.clone();
        }

        // Propagate encoding stats to ScanStats if we have a handle.
        // Full extraction: replace root totals (this scanned every file).
        if let Some(ref stats_handle) = self.scan_stats
            && let Ok(mut stats) = stats_handle.write()
        {
            stats.set_encoding_stats(slug, enc_stats);
        }

        tracing::info!(
            "TreeSitterConsumer: root '{}' extracted: {} nodes, {} edges",
            slug,
            extraction.nodes.len(),
            extraction.edges.len(),
        );

        Ok(vec![ExtractionEvent::RootExtracted {
            slug: slug.clone(),
            path: path.clone(),
            nodes: std::sync::Arc::from(extraction.nodes.into_boxed_slice()),
            edges: std::sync::Arc::from(extraction.edges.into_boxed_slice()),
            // Single-root extraction: this root is always dirty.
            dirty_slugs: Some(std::collections::HashSet::from([slug.clone()])),
        }])
    }

    /// `TreeSitterConsumer` reads the filesystem — its output depends on file contents
    /// beyond the event payload. The bus must not cache its output; every scan must
    /// trigger a fresh read so file edits are detected.
    fn is_cacheable(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// LanguageAccumulatorConsumer
// ---------------------------------------------------------------------------

/// Groups extracted nodes by language, emits one `LanguageDetected` per language.
///
/// Subscribes to: `RootExtracted`
/// Emits: `LanguageDetected` (one per language found)
pub struct LanguageAccumulatorConsumer;

#[async_trait]
impl ExtractionConsumer for LanguageAccumulatorConsumer {
    fn name(&self) -> &str { "language_accumulator" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootExtracted]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::RootExtracted { slug, nodes, dirty_slugs, .. } = event else {
            return Ok(vec![]);
        };

        // Only count languages from nodes whose root is in dirty_slugs.
        // Clean-root nodes are in the set for post-extraction passes but should NOT
        // trigger new LSP enrichment.
        // - `None` = all roots dirty (first-run / cache-hit LSP paths)
        // - `Some(set)` = only roots in set are dirty; `Some(empty)` = skip all LSP
        let dirty_set = dirty_slugs.as_ref();

        // Group nodes by language using BTreeMap for deterministic emission order.
        // BTreeMap ensures LanguageDetected events always fire in alphabetical language
        // order (go, python, rust, typescript, ...) across runs — stable for testing
        // and for Phase 3+ when consumers may depend on relative event ordering.
        //
        // Nodes are cloned into per-language buckets. The event payload uses Arc<[Node]>
        // to avoid O(N) copies when multiple consumers receive the same LanguageDetected.
        let mut by_lang: std::collections::BTreeMap<String, Vec<Node>> =
            std::collections::BTreeMap::new();
        for node in nodes.iter() {
            if node.language.is_empty() {
                continue;
            }
            // Skip nodes from clean roots — they already have LSP edges from cache.
            if let Some(set) = dirty_set
                && !set.contains(&node.id.root)
            {
                continue;
            }
            by_lang.entry(node.language.clone()).or_default().push(node.clone());
        }

        let mut events: Vec<ExtractionEvent> = Vec::with_capacity(by_lang.len());
        for (language, lang_nodes) in by_lang {
            tracing::debug!(
                "LanguageAccumulatorConsumer: root '{}' language '{}': {} nodes (dirty_slugs: {:?})",
                slug,
                language,
                lang_nodes.len(),
                dirty_slugs,
            );
            events.push(ExtractionEvent::LanguageDetected {
                slug: slug.clone(),
                language,
                nodes: std::sync::Arc::from(lang_nodes.into_boxed_slice()),
            });
        }

        Ok(events)
    }
}

// ---------------------------------------------------------------------------
// ApiLinkConsumer
// ---------------------------------------------------------------------------

/// Links HTTP handler nodes to route definitions by matching path strings.
///
/// Subscribes to: `AllEnrichmentsDone`
/// Emits: nothing (no-op subscription slot)
///
/// This consumer establishes the event-driven subscription slot for `api_link_pass`.
/// The actual pass logic and its output are handled exclusively by `EnrichmentFinalizer`,
/// which runs `api_link_pass` as part of its ordered pass sequence and includes
/// the resulting edges in `PassesComplete`. No duplicate computation occurs here.
pub struct ApiLinkConsumer;

#[async_trait]
impl ExtractionConsumer for ApiLinkConsumer {
    fn name(&self) -> &str { "api_link" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::AllEnrichmentsDone]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::AllEnrichmentsDone { slug, .. } = event else {
            return Ok(vec![]);
        };
        // `EnrichmentFinalizer` is the authoritative runner for api_link_pass and
        // includes its output in `PassesComplete`. This consumer is a subscription
        // slot — it signals that the api_link pass participates in the event-driven
        // pipeline. No pass re-execution here avoids duplicate work.
        tracing::debug!("ApiLinkConsumer: root '{}' AllEnrichmentsDone received (pass runs in EnrichmentFinalizer)", slug);
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// TestedByConsumer
// ---------------------------------------------------------------------------

/// Emits `TestedBy` edges between test functions and the functions they test.
///
/// Subscribes to: `AllEnrichmentsDone`
/// Emits: nothing (no-op subscription slot)
///
/// This consumer establishes the event-driven subscription slot for `tested_by_pass`.
/// The actual pass logic and its output are handled exclusively by `EnrichmentFinalizer`,
/// which runs `tested_by_pass` as part of its ordered pass sequence and includes
/// the resulting edges in `PassesComplete`. No duplicate computation occurs here.
pub struct TestedByConsumer;

#[async_trait]
impl ExtractionConsumer for TestedByConsumer {
    fn name(&self) -> &str { "tested_by" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::AllEnrichmentsDone]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::AllEnrichmentsDone { slug, .. } = event else {
            return Ok(vec![]);
        };
        // `EnrichmentFinalizer` is the authoritative runner for tested_by_pass and
        // includes its output in `PassesComplete`. This consumer is a subscription
        // slot — it signals that the tested_by pass participates in the event-driven
        // pipeline. No pass re-execution here avoids duplicate work.
        tracing::debug!("TestedByConsumer: root '{}' AllEnrichmentsDone received (pass runs in EnrichmentFinalizer)", slug);
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// FastapiRouterPrefixConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `FrameworkDetected("fastapi")` — signals that FastAPI is in use.
///
/// **ADR pattern:** Framework-gated pass as a consumer — wakes only when its
/// framework fires, never polls context.
///
/// The actual prefix patching (prepending `APIRouter(prefix=...)` and
/// `include_router(..., prefix=...)` values to `http_path` on ApiEndpoint nodes)
/// runs inside `EnrichmentFinalizer`, which has access to the full node set and
/// `root_pairs`. This consumer establishes the event-driven subscription slot and
/// emits `PassComplete` as a signal that the fastapi framework was detected.
///
/// The mis-wiring fixed here (issue #537): `FastapiRouterPrefixPass` previously
/// ran unconditionally before framework detection. It now subscribes to
/// `FrameworkDetected("fastapi")` so it is gated on actual FastAPI detection.
///
/// Subscribes to: `FrameworkDetected`
/// Emits: `PassComplete`
pub struct FastapiRouterPrefixConsumer;

#[async_trait]
impl ExtractionConsumer for FastapiRouterPrefixConsumer {
    fn name(&self) -> &str { "fastapi_router_prefix" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::FrameworkDetected { slug, framework, .. } = event else {
            return Ok(vec![]);
        };
        if framework != "fastapi" {
            return Ok(vec![]);
        }
        tracing::debug!(
            "FastapiRouterPrefixConsumer: root '{}' fastapi detected — prefix patching will run in EnrichmentFinalizer",
            slug,
        );
        Ok(vec![ExtractionEvent::PassComplete {
            pass_name: "fastapi_router_prefix",
            added_nodes: 0,
            added_edges: 0,
        }])
    }
}

// ---------------------------------------------------------------------------
// SdkPathInferenceConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `FrameworkDetected("fastapi")` — signals that FastAPI is in use.
///
/// **ADR pattern:** Framework-gated pass as a consumer — wakes only when its
/// framework fires, never polls context.
///
/// The actual SDK path inference (inferring full HTTP paths for FastAPI endpoints
/// from generated TypeScript/JavaScript SDK Const nodes) runs inside
/// `EnrichmentFinalizer`, which has access to the full node set. This consumer
/// establishes the event-driven subscription slot and emits `PassComplete` as a
/// signal that the fastapi framework was detected.
///
/// Both `fastapi_router_prefix_pass` and `sdk_path_inference_pass` are gated on
/// FastAPI detection in `EnrichmentFinalizer::run_passes`. On repos without FastAPI
/// (e.g., Rust-only, plain Python, Next.js-only), neither pass is ever invoked —
/// zero overhead, not "fast-path exit".
///
/// Subscribes to: `FrameworkDetected`
/// Emits: `PassComplete`
pub struct SdkPathInferenceConsumer;

#[async_trait]
impl ExtractionConsumer for SdkPathInferenceConsumer {
    fn name(&self) -> &str { "sdk_path_inference" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::FrameworkDetected { slug, framework, .. } = event else {
            return Ok(vec![]);
        };
        if framework != "fastapi" {
            return Ok(vec![]);
        }
        tracing::debug!(
            "SdkPathInferenceConsumer: root '{}' fastapi detected — SDK path inference will run in EnrichmentFinalizer",
            slug,
        );
        Ok(vec![ExtractionEvent::PassComplete {
            pass_name: "sdk_path_inference",
            added_nodes: 0,
            added_edges: 0,
        }])
    }
}

// ---------------------------------------------------------------------------
// EnrichmentFinalizer
// ---------------------------------------------------------------------------

/// Runs all enrichment passes on the LSP-enriched graph and emits `PassesComplete`.
///
/// This consumer is the "last consumer in the chain" — it always emits `PassesComplete`
/// so downstream consumers (SubsystemConsumer, LanceDBConsumer) have a well-defined
/// trigger point.
///
/// Subscribes to: `AllEnrichmentsDone`
/// Emits: `FrameworkDetected` (one per detected framework) + `PassesComplete`
///
/// **Pass execution order (ordering-sensitive):**
/// 1. Merge tree-sitter + LSP nodes/edges and apply metadata patches
/// 2. `framework_detection_pass` — detects frameworks from Import nodes.
///    Must run FIRST so all subsequent passes can gate on detected frameworks.
///    Returns `detected_frameworks` set and virtual framework nodes.
/// 3. `fastapi_router_prefix_pass` — rewrites `http_path` on FastAPI ApiEndpoint nodes
///    with explicit `prefix=` args. Gated on `detected_frameworks.contains("fastapi")`.
///    Must run BEFORE `api_link_pass` (PR #528).
/// 4. `sdk_path_inference_pass` — infers full paths for FastAPI endpoints still unresolved
///    (chained routers without explicit `prefix=` args); uses SDK Const nodes (#517).
///    Gated on `detected_frameworks.contains("fastapi")` — zero invocations on non-FastAPI repos.
/// 5. `api_link_pass` — links HTTP handlers to route definitions
/// 6. `openapi_sdk_link_pass` — links SDK functions to OpenAPI spec operations
/// 7. `manifest_pass` — package.json / Cargo.toml dependency nodes
/// 8. `tested_by_pass` — naming-convention test edges
/// 9. `import_calls_pass` — resolves bare function calls via import nodes
/// 10. `directory_module_pass` — directory-level module nodes
/// 11. Framework-gated passes: `pubsub_pass`, `websocket_pass`, `nextjs_routing_pass`,
///     `grpc_client_calls_pass`, `extractor_config_pass` — gate on detected_frameworks
///
/// **Why passes run here, not in individual consumers:**
/// Each pass needs either the full node set (not available in per-event payloads) or
/// must contribute edges to the `PassesComplete` payload. The event bus in Phase 2/3 is
/// synchronous and `Arc<[T]>` payloads are immutable — there is no shared mutable accumulator
/// for individual consumers to append into. `EnrichmentFinalizer` is the aggregation point.
/// `ApiLinkConsumer` and `TestedByConsumer` are no-op subscription slots that establish the
/// event-driven wiring; `EnrichmentFinalizer` is the sole authoritative runner for those passes.
/// `FastapiRouterPrefixConsumer` and `SdkPathInferenceConsumer` are signal consumers that
/// fire when fastapi is detected.
pub struct EnrichmentFinalizer {
    root_pairs: Vec<(String, PathBuf)>,
    primary_slug: String,
}

impl EnrichmentFinalizer {
    pub fn new(root_pairs: Vec<(String, PathBuf)>, primary_slug: String) -> Self {
        Self { root_pairs, primary_slug }
    }
}

#[async_trait]
impl ExtractionConsumer for EnrichmentFinalizer {
    fn name(&self) -> &str { "enrichment_finalizer" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::AllEnrichmentsDone]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::AllEnrichmentsDone { slug, nodes, edges, lsp_edges, lsp_nodes, updated_nodes } = event else {
            return Ok(vec![]);
        };

        // Step 1: Merge tree-sitter nodes/edges with LSP-enriched additions.
        // Post-extraction passes operate on the full LSP-enriched graph.
        let mut all_nodes = nodes.to_vec();
        all_nodes.extend_from_slice(lsp_nodes);
        let mut all_edges = edges.to_vec();
        all_edges.extend_from_slice(lsp_edges);

        // Apply metadata patches from LSP enrichment (e.g., inferred types from inlay hints).
        if !updated_nodes.is_empty() {
            let node_pos: std::collections::HashMap<String, usize> = all_nodes
                .iter()
                .enumerate()
                .map(|(i, n)| (n.stable_id(), i))
                .collect();
            for (node_id, patches) in updated_nodes.iter() {
                if let Some(&idx) = node_pos.get(node_id) {
                    let node = &mut all_nodes[idx];
                    for (key, value) in patches {
                        node.metadata.insert(key.clone(), value.clone());
                    }
                }
            }
            tracing::debug!(
                "EnrichmentFinalizer: root '{}' applied {} metadata patch(es) from LSP enrichment",
                slug,
                updated_nodes.len(),
            );
        }

        self.run_passes(slug, all_nodes, all_edges)
    }

    /// `EnrichmentFinalizer` reads the filesystem (api_link, manifest, etc.).
    /// Its output depends on file contents beyond the event payload. Non-cacheable.
    fn is_cacheable(&self) -> bool {
        false
    }
}

impl EnrichmentFinalizer {
    fn run_passes(
        &self,
        slug: &str,
        mut all_nodes: Vec<Node>,
        mut all_edges: Vec<crate::graph::Edge>,
    ) -> anyhow::Result<Vec<ExtractionEvent>> {
        let nodes_before = all_nodes.len();
        let edges_before = all_edges.len();

        // Step 2: framework_detection — detects frameworks from Import nodes.
        // Must run FIRST so downstream passes can gate on the detected framework set.
        // Only needs Import nodes (present after tree-sitter extraction). Running here
        // means framework-gated passes (Steps 3, 4, 11) are zero-invocation on repos
        // without the matching framework — no "fast-path exit" gates, just never called.
        let detected_frameworks: HashSet<String>;
        {
            let result = crate::extract::framework_detection::framework_detection_pass(
                &all_nodes,
                &self.primary_slug,
            );
            detected_frameworks = result.detected_frameworks;
            if !result.nodes.is_empty() {
                all_nodes.extend(result.nodes);
            }
        }

        // Step 3: fastapi_router_prefix — rewrites `http_path` on FastAPI ApiEndpoint nodes
        // with explicit `prefix=` args. Gated on FastAPI detection: zero invocations on
        // repos without FastAPI. Must run BEFORE api_link_pass (PR #528).
        if detected_frameworks.contains("fastapi") {
            crate::extract::fastapi_router_prefix::fastapi_router_prefix_pass(
                &mut all_nodes,
                &self.root_pairs,
            );
        }

        // Step 4: sdk_path_inference — infers full HTTP paths for FastAPI endpoints that
        // remain unresolved after step 3 (chained routers without explicit prefix= args).
        // Uses URL paths from generated SDK Const nodes as the authoritative source.
        // Gated on FastAPI detection: zero invocations on non-FastAPI repos.
        // Must run BEFORE api_link_pass so the full paths participate in URL-string matching.
        if detected_frameworks.contains("fastapi") {
            crate::extract::sdk_path_inference::sdk_path_inference_pass(&mut all_nodes);
        }

        // Step 5: api_link — links HTTP handlers to route definitions.
        {
            let new_edges = crate::extract::api_link::api_link_pass(&all_nodes);
            if !new_edges.is_empty() {
                all_edges.extend(new_edges);
            }
        }

        // Step 6: openapi_sdk_link — links SDK functions to OpenAPI spec operations.
        {
            let new_edges = crate::extract::openapi_sdk_link::openapi_sdk_link_pass(&all_nodes);
            if !new_edges.is_empty() {
                all_edges.extend(new_edges);
            }
        }

        // Step 7: manifest — package.json / Cargo.toml / pyproject.toml dependency nodes.
        {
            let result = crate::extract::manifest::manifest_pass(&self.root_pairs);
            if !result.nodes.is_empty() || !result.edges.is_empty() {
                all_nodes.extend(result.nodes);
                all_edges.extend(result.edges);
            }
        }

        // Step 8: tested_by — naming-convention test edges (test_foo → foo).
        {
            let new_edges = crate::extract::naming_convention::tested_by_pass(&all_nodes);
            if !new_edges.is_empty() {
                all_edges.extend(new_edges);
            }
        }

        // Step 9: import_calls — resolves bare function calls via import nodes.
        {
            let new_edges = crate::extract::import_calls::import_calls_pass(&all_nodes);
            if !new_edges.is_empty() {
                all_edges.extend(new_edges);
            }
        }

        // Step 10: directory_module — directory-level module nodes.
        {
            let result = crate::extract::directory_module::directory_module_pass(&all_nodes);
            if !result.nodes.is_empty() || !result.edges.is_empty() {
                all_nodes.extend(result.nodes);
                all_edges.extend(result.edges);
            }
        }

        // Step 11: framework-gated passes — run only when the relevant framework is detected.
        // pubsub
        if crate::extract::pubsub::should_run(&detected_frameworks) {
            let result = crate::extract::pubsub::pubsub_pass(
                &all_nodes,
                &detected_frameworks,
                &self.primary_slug,
            );
            if !result.nodes.is_empty() || !result.edges.is_empty() {
                all_nodes.extend(result.nodes);
                all_edges.extend(result.edges);
            }
        }
        // websocket
        if crate::extract::websocket::should_run(&detected_frameworks) {
            let result = crate::extract::websocket::websocket_pass(
                &all_nodes,
                &detected_frameworks,
                &self.primary_slug,
            );
            if !result.nodes.is_empty() || !result.edges.is_empty() {
                all_nodes.extend(result.nodes);
                all_edges.extend(result.edges);
            }
        }
        // nextjs_routing — gated on Next.js detection.
        //
        // Two detection signals (either suffices):
        // 1. Import-based: `framework_detection_pass` saw `next/*` imports.
        // 2. Filesystem-based: any root contains `pages/`, `app/`, or
        //    `next.config.{js,ts,mjs}` — covers repos whose API route files
        //    import nothing from `next/` directly (e.g., plain handler functions).
        //
        // Pure `has_ts_js` (any TS/JS file) is NOT used — that would run on
        // plain React, Angular, or Vue repos that have no Next.js structure.
        {
            let import_detected = detected_frameworks.contains("nextjs-app-router");
            let fs_detected = !import_detected && (
                // Check root-level paths (single-root repos)
                self.root_pairs.iter().any(|(_, root_path)| {
                    root_path.join("pages").is_dir()
                        || root_path.join("app").is_dir()
                        || root_path.join("src/pages").is_dir()
                        || root_path.join("src/app").is_dir()
                        || root_path.join("next.config.js").exists()
                        || root_path.join("next.config.ts").exists()
                        || root_path.join("next.config.mjs").exists()
                })
                // Check node file paths for Next.js patterns (monorepo-aware).
                // If any extracted node has a file path containing `/app/api/`
                // or `/pages/api/`, the project likely has a Next.js app in a
                // subdirectory that the root-level check missed.
                || all_nodes.iter().any(|n| {
                    let fp = n.id.file.to_string_lossy();
                    fp.contains("/app/api/") || fp.contains("/pages/api/")
                        || fp.starts_with("app/api/") || fp.starts_with("pages/api/")
                })
                // Check immediate subdirectories for next.config or app/pages dirs.
                // This catches monorepo layouts like `client/app/` or `web/next.config.js`.
                || self.root_pairs.iter().any(|(_, root_path)| {
                    has_nextjs_in_subdirs(root_path)
                })
            );
            if import_detected || fs_detected {
                let result = crate::extract::nextjs_routing::nextjs_routing_pass(
                    &self.root_pairs,
                    &all_nodes,
                );
                if !result.nodes.is_empty() || !result.edges.is_empty() {
                    all_nodes.extend(result.nodes);
                    all_edges.extend(result.edges);
                }
            }
        }
        // grpc_client_calls
        if crate::extract::grpc::should_run(&detected_frameworks) {
            let new_edges = crate::extract::grpc::grpc_client_calls_pass(&all_nodes);
            if !new_edges.is_empty() {
                all_edges.extend(new_edges);
            }
        }
        // extractor_config — config-driven boundary detection per workspace root
        for (root_slug, root_path) in &self.root_pairs {
            let configs = crate::extract::extractor_config::load_extractor_configs(root_path.as_path());
            if configs.is_empty() {
                continue;
            }
            let root_nodes: Vec<_> = all_nodes.iter().filter(|n| &n.id.root == root_slug).cloned().collect();
            if root_nodes.is_empty() {
                continue;
            }
            let result = crate::extract::extractor_config::extractor_config_pass_with_configs(
                &root_nodes,
                root_slug.as_str(),
                &configs,
            );
            if !result.nodes.is_empty() || !result.edges.is_empty() {
                all_nodes.extend(result.nodes);
                all_edges.extend(result.edges);
            }
        }

        let added_nodes = all_nodes.len().saturating_sub(nodes_before);
        let added_edges = all_edges.len().saturating_sub(edges_before);
        tracing::info!(
            "EnrichmentFinalizer: root '{}' passes complete: +{} node(s), +{} edge(s), {} framework(s)",
            slug,
            added_nodes,
            added_edges,
            detected_frameworks.len(),
        );

        let mut follow_ons: Vec<ExtractionEvent> = Vec::new();

        // Emit FrameworkDetected for each detected framework.
        // Sort framework names for deterministic fan-out ordering.
        let mut frameworks: Vec<String> = detected_frameworks.iter().cloned().collect();
        frameworks.sort_unstable();
        for framework in &frameworks {
            let fw_nodes: Vec<Node> = all_nodes
                .iter()
                .filter(|n| {
                    n.metadata.get("framework").map(|f| f == framework).unwrap_or(false)
                })
                .cloned()
                .collect();
            follow_ons.push(ExtractionEvent::FrameworkDetected {
                slug: slug.to_string(),
                framework: framework.clone(),
                nodes: std::sync::Arc::from(fw_nodes.into_boxed_slice()),
            });
        }

        // Always emit PassesComplete with the enriched graph.
        let nodes_arc = std::sync::Arc::from(all_nodes.into_boxed_slice());
        let edges_arc = std::sync::Arc::from(all_edges.into_boxed_slice());
        follow_ons.push(ExtractionEvent::PassesComplete {
            slug: slug.to_string(),
            nodes: nodes_arc,
            edges: edges_arc,
            detected_frameworks,
        });

        Ok(follow_ons)
    }
}

// ---------------------------------------------------------------------------
// Next.js monorepo detection helper
// ---------------------------------------------------------------------------

/// Check immediate subdirectories of `root_path` for Next.js indicators.
///
/// In a monorepo, `app/`, `pages/`, or `next.config.*` may live under a
/// subdirectory like `client/` or `web/` rather than at the root.
/// This scans one level of subdirectories (skipping vendor/noise dirs).
fn has_nextjs_in_subdirs(root_path: &std::path::Path) -> bool {
    let Ok(rd) = std::fs::read_dir(root_path) else { return false; };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.')
            || name_str == "node_modules"
            || name_str == "target"
            || name_str == "dist"
            || name_str == "build"
            || name_str == "vendor"
        {
            continue;
        }
        if path.join("app").is_dir()
            || path.join("pages").is_dir()
            || path.join("src/app").is_dir()
            || path.join("src/pages").is_dir()
            || path.join("next.config.js").exists()
            || path.join("next.config.ts").exists()
            || path.join("next.config.mjs").exists()
        {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// FrameworkDetectionConsumer
// ---------------------------------------------------------------------------

/// Logs framework detection events. Framework-gated passes subscribe to
/// `FrameworkDetected` and check `event.framework` to filter.
///
/// This consumer is a diagnostic observer. The actual framework-gated pass logic
/// (pubsub, websocket, nextjs, grpc, extractor_config) runs inside `EnrichmentFinalizer`
/// where the full node set is available. Per-framework stub consumers
/// (`NextjsRoutingConsumer`, `PubSubConsumer`, `WebSocketConsumer`,
/// `FastapiRouterPrefixConsumer`, `SdkPathInferenceConsumer`) subscribe here for
/// monitoring/signalling.
pub struct FrameworkDetectionConsumer;

#[async_trait]
impl ExtractionConsumer for FrameworkDetectionConsumer {
    fn name(&self) -> &str { "framework_detection" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::FrameworkDetected { slug, framework, nodes } = event else {
            return Ok(vec![]);
        };
        tracing::info!(
            "FrameworkDetectionConsumer: root '{}' framework '{}' detected ({} nodes)",
            slug,
            framework,
            nodes.len(),
        );
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// NextjsRoutingConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `FrameworkDetected("nextjs-app-router")` — signals that Next.js
/// app-router conventions are in use.
///
/// **ADR pattern:** Framework-gated pass as a consumer — wakes only when its
/// framework fires, never polls context. On repos without Next.js this consumer
/// never fires — zero invocations, zero overhead.
///
/// The actual pass (`nextjs_routing_pass`) runs inside `EnrichmentFinalizer`, which
/// has access to the full node set and `root_pairs`. This consumer establishes the
/// event-driven subscription slot and emits `PassComplete` as a monitoring signal.
///
/// The old `has_ts_js` fallback (any TypeScript/JavaScript node) has been replaced
/// with a filesystem-based secondary signal in `EnrichmentFinalizer`: if any root
/// contains `pages/`, `app/`, `src/pages/`, `src/app/`, or `next.config.{js,ts,mjs}`,
/// the routing pass runs even without `next/*` imports. This covers repos whose API
/// route handlers don't import directly from `next/` (plain handler functions).
///
/// Subscribes to: `FrameworkDetected`
/// Emits: `PassComplete`
pub struct NextjsRoutingConsumer {
    /// Stored for Phase 4+ when nextjs_routing_pass is promoted out of EnrichmentFinalizer
    /// and becomes a fully independent consumer with access to the full node set.
    #[allow(dead_code)]
    root_pairs: Vec<(String, PathBuf)>,
}

impl NextjsRoutingConsumer {
    pub fn new(root_pairs: Vec<(String, PathBuf)>) -> Self {
        Self { root_pairs }
    }
}

#[async_trait]
impl ExtractionConsumer for NextjsRoutingConsumer {
    fn name(&self) -> &str { "nextjs_routing" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::FrameworkDetected { slug, framework, .. } = event else {
            return Ok(vec![]);
        };

        // Only wake for nextjs-app-router. Plain TS/JS repos without Next.js produce
        // no page-route ApiEndpoint nodes. The filesystem-based fallback in
        // EnrichmentFinalizer covers repos with Next.js directory structure but no
        // `next/*` imports.
        if framework != "nextjs-app-router" {
            return Ok(vec![]);
        }

        // nextjs_routing_pass needs the full node set, not just the framework nodes.
        // This consumer emits PassComplete as a signal; the actual pass logic runs inside
        // EnrichmentFinalizer where the full node set is available.
        tracing::debug!(
            "NextjsRoutingConsumer: root '{}' next.js routing triggered (framework='{}')",
            slug,
            framework,
        );
        Ok(vec![ExtractionEvent::PassComplete {
            pass_name: "nextjs_routing",
            added_nodes: 0,
            added_edges: 0,
        }])
    }
}

// ---------------------------------------------------------------------------
// PubSubConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `FrameworkDetected` and runs pub/sub pass when a message-broker
/// framework is detected (kafka, celery, pika, redis).
///
/// **ADR pattern:** Framework-gated pass — subscribes to `FrameworkDetected`,
/// checks `framework` in `on_event`. No broker knowledge in RNA core: the
/// framework name comes from `framework_detection_pass` which reads import nodes.
///
/// Subscribes to: `FrameworkDetected`
/// Emits: `PassComplete`
pub struct PubSubConsumer;

#[async_trait]
impl ExtractionConsumer for PubSubConsumer {
    fn name(&self) -> &str { "pubsub" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::FrameworkDetected { slug, framework, .. } = event else {
            return Ok(vec![]);
        };
        // Only fire for broker frameworks. The actual pass logic runs inside
        // EnrichmentFinalizer where the full node set is available. This consumer
        // establishes the event-driven subscription slot for monitoring.
        let is_broker = matches!(
            framework.as_str(),
            "kafka-python" | "confluent-kafka" | "celery" | "pika" | "redis"
        );
        if !is_broker {
            return Ok(vec![]);
        }
        tracing::debug!("PubSubConsumer: root '{}' broker '{}' detected", slug, framework);
        Ok(vec![ExtractionEvent::PassComplete {
            pass_name: "pubsub",
            added_nodes: 0,
            added_edges: 0,
        }])
    }
}

// ---------------------------------------------------------------------------
// WebSocketConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `FrameworkDetected("socketio")` and handles WebSocket passes.
///
/// Subscribes to: `FrameworkDetected`
/// Emits: `PassComplete`
pub struct WebSocketConsumer;

#[async_trait]
impl ExtractionConsumer for WebSocketConsumer {
    fn name(&self) -> &str { "websocket" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::FrameworkDetected { slug, framework, .. } = event else {
            return Ok(vec![]);
        };
        if framework != "socketio" {
            return Ok(vec![]);
        }
        tracing::debug!("WebSocketConsumer: root '{}' socketio detected", slug);
        Ok(vec![ExtractionEvent::PassComplete {
            pass_name: "websocket",
            added_nodes: 0,
            added_edges: 0,
        }])
    }
}

// ---------------------------------------------------------------------------
// OpenApiConsumer (#465)
// ---------------------------------------------------------------------------

/// Subscribes to `RootExtracted`, runs OpenAPI bidirectional linking.
///
/// Joins SDK functions to OpenAPI spec operationIds.
/// Phase 2: stub that establishes the subscription slot.
/// Full implementation tracked by #465.
///
/// Subscribes to: `RootExtracted`
/// Emits: `PassComplete`
pub struct OpenApiConsumer;

#[async_trait]
impl ExtractionConsumer for OpenApiConsumer {
    fn name(&self) -> &str { "openapi_bidirectional" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootExtracted]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::RootExtracted { slug, nodes, .. } = event else {
            return Ok(vec![]);
        };
        // Only run if there are OpenAPI-specific nodes present. Use the dedicated
        // "openapi" language tag set by the OpenApiExtractor — not generic yaml/json
        // which fires for any config file (package.json, .github/workflows, etc.).
        let has_openapi = nodes.iter().any(|n| n.language == "openapi");
        if !has_openapi {
            return Ok(vec![]);
        }
        tracing::debug!("OpenApiConsumer: root '{}' has OpenAPI nodes (stub)", slug);
        Ok(vec![ExtractionEvent::PassComplete {
            pass_name: "openapi_bidirectional",
            added_nodes: 0,
            added_edges: 0,
        }])
    }
}

// ---------------------------------------------------------------------------
// GrpcConsumer (#466)
// ---------------------------------------------------------------------------

/// Subscribes to `RootExtracted`, parses .proto files for gRPC service/method edges.
///
/// Phase 2: stub. Full implementation tracked by #466.
///
/// Subscribes to: `RootExtracted`
/// Emits: `PassComplete`
pub struct GrpcConsumer;

#[async_trait]
impl ExtractionConsumer for GrpcConsumer {
    fn name(&self) -> &str { "grpc_proto" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootExtracted]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::RootExtracted { slug, nodes, .. } = event else {
            return Ok(vec![]);
        };
        let has_proto = nodes.iter().any(|n| n.language == "protobuf");
        if !has_proto {
            return Ok(vec![]);
        }
        tracing::debug!("GrpcConsumer: root '{}' has .proto files (stub)", slug);
        Ok(vec![ExtractionEvent::PassComplete {
            pass_name: "grpc_proto",
            added_nodes: 0,
            added_edges: 0,
        }])
    }
}

// ---------------------------------------------------------------------------
// CustomExtractorConsumer (#468)
// ---------------------------------------------------------------------------

/// Subscribes to `FrameworkDetected` and runs config-driven passes from `.oh/extractors/*.toml`.
///
/// Phase 2: stub that establishes the subscription slot.
/// Full implementation tracked by #468.
///
/// Subscribes to: `FrameworkDetected`
/// Emits: `PassComplete`
///
/// # Content-addressed versioning
///
/// `CustomExtractorConsumer` derives its `version()` from `blake3(config_file_contents)`.
/// When the `.oh/extractors/*.toml` file changes, `version()` changes automatically —
/// no manual `EXTRACTION_VERSION` bump required.
pub struct CustomExtractorConsumer {
    /// Framework name this consumer is configured for.
    pub framework: String,
    /// Slug identifying this config (for diagnostics).
    pub config_name: String,
    /// Raw bytes of the `.oh/extractors/*.toml` config file.
    /// Used to derive `version()` via `blake3` so config edits auto-invalidate the cache.
    pub config_bytes: Vec<u8>,
}

impl CustomExtractorConsumer {
    /// Create a `CustomExtractorConsumer` by reading the config file from disk.
    ///
    /// If the file cannot be read, `config_bytes` is set to an empty `Vec` which
    /// produces a stable but generic version (`0` for the first 8 bytes of blake3(b"")`).
    pub fn from_file(framework: String, config_name: String, config_path: &std::path::Path) -> Self {
        let config_bytes = match std::fs::read(config_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(
                    "CustomExtractorConsumer '{}': could not read config file '{}': {} — \
                     using empty config_bytes (version will be blake3(b\"\"))",
                    config_name,
                    config_path.display(),
                    e,
                );
                Vec::new()
            }
        };
        Self { framework, config_name, config_bytes }
    }
}

#[async_trait]
impl ExtractionConsumer for CustomExtractorConsumer {
    fn name(&self) -> &str { "custom_extractor" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::FrameworkDetected { slug, framework, .. } = event else {
            return Ok(vec![]);
        };
        if framework != &self.framework {
            return Ok(vec![]);
        }
        tracing::debug!(
            "CustomExtractorConsumer '{}': root '{}' framework '{}' matched (stub)",
            self.config_name,
            slug,
            framework,
        );
        Ok(vec![ExtractionEvent::PassComplete {
            pass_name: "custom_extractor",
            added_nodes: 0,
            added_edges: 0,
        }])
    }

    /// Version derived from blake3 of the config file bytes.
    ///
    /// When the `.oh/extractors/*.toml` changes, `version()` changes automatically —
    /// no manual bump needed. Uses the first 8 bytes of the hash as a `u64`.
    fn version(&self) -> u64 {
        let hash = blake3::hash(&self.config_bytes);
        let bytes = hash.as_bytes();
        u64::from_le_bytes(bytes[..8].try_into().expect("blake3 output >= 8 bytes"))
    }
}

// ---------------------------------------------------------------------------
// LspConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `LanguageDetected` and runs real LSP enrichment for the given language.
///
/// **Phase 4 async implementation.** Holds an `Arc<dyn Enricher>` for its language.
/// In `on_event(LanguageDetected)`, directly awaits `enricher.enrich()` — no
/// `block_in_place` needed since the bus is now async.
///
/// Per the ADR: "Consumer of LanguageDetected(lang, nodes): LSP enrichers — ALL fire
/// concurrently, one per language." The async bus enables future parallel execution
/// across languages. Currently the bus awaits consumers sequentially in registration
/// order.
///
/// Subscribes to: `LanguageDetected`
/// Emits: `EnrichmentComplete` (with real edges and virtual nodes from LSP)
pub struct LspConsumer {
    /// Which language this consumer handles (e.g., "rust", "python", "typescript").
    pub language: String,
    /// The LSP enricher for this language. `Arc` allows sharing across bus fan-out clones.
    pub enricher: Arc<dyn crate::extract::Enricher>,
    /// Repo root for the LSP server startup working directory.
    pub repo_root: PathBuf,
    /// Per-root LSP root overrides for monorepo setups.
    pub lsp_roots: Arc<Vec<(String, PathBuf)>>,
}

#[async_trait]
impl ExtractionConsumer for LspConsumer {
    fn name(&self) -> &str { "lsp" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::LanguageDetected]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::LanguageDetected { slug, language, nodes } = event else {
            return Ok(vec![]);
        };
        if language != &self.language {
            return Ok(vec![]);
        }

        tracing::info!(
            "LspConsumer({}): root '{}' — {} nodes, starting LSP enrichment",
            self.language,
            slug,
            nodes.len(),
        );

        let enricher = Arc::clone(&self.enricher);
        let nodes_vec: Vec<Node> = nodes.to_vec();
        let repo_root = self.repo_root.clone();
        let lsp_roots = Arc::clone(&self.lsp_roots);

        // Configure startup root for monorepo: prefer the most-specific lsp_root
        // that matches the language's config file (e.g., tsconfig.json for TypeScript).
        if !lsp_roots.is_empty() {
            let index = crate::graph::index::GraphIndex::new();
            let best = crate::extract::pick_lsp_root_for_nodes(
                &nodes_vec,
                &repo_root,
                &lsp_roots,
                enricher.config_file_hint(),
            );
            if best != repo_root.as_path() {
                enricher.set_startup_root(best.to_path_buf());
            }
            drop(index);
        }

        // Run LSP enrichment natively async — the bus is async so we can await
        // directly without block_in_place.
        let enrichment_result = {
            // Build an index from the nodes visible to this enricher so that
            // enrichers that resolve or deduplicate through the index see a
            // populated graph rather than an empty one.
            let mut index = crate::graph::index::GraphIndex::new();
            for node in &nodes_vec {
                index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
            }
            enricher.enrich(&nodes_vec, &index, &repo_root).await
        };

        match enrichment_result {
            Ok(enrichment) => {
                tracing::info!(
                    "LspConsumer({}): root '{}' enrichment complete: {} edges, {} virtual nodes, {} patches",
                    self.language,
                    slug,
                    enrichment.added_edges.len(),
                    enrichment.new_nodes.len(),
                    enrichment.updated_nodes.len(),
                );
                Ok(vec![ExtractionEvent::EnrichmentComplete {
                    slug: slug.clone(),
                    language: language.clone(),
                    added_edges: Arc::from(enrichment.added_edges.into_boxed_slice()),
                    new_nodes: Arc::from(enrichment.new_nodes.into_boxed_slice()),
                    updated_nodes: Arc::from(enrichment.updated_nodes.into_boxed_slice()),
                    server_name: Some(self.enricher.name().to_string()),
                    error_count: enrichment.error_count,
                    aborted: enrichment.aborted,
                }])
            }
            Err(e) => {
                // LSP enrichment failure is non-fatal: emit EnrichmentComplete with
                // empty results so AllEnrichmentsGate can still proceed. The graph
                // will have tree-sitter-only edges for this language but won't be stuck.
                tracing::warn!(
                    "LspConsumer({}): root '{}' enrichment failed (non-fatal): {}",
                    self.language,
                    slug,
                    e,
                );
                Ok(vec![ExtractionEvent::EnrichmentComplete {
                    slug: slug.clone(),
                    language: language.clone(),
                    added_edges: Arc::from([]),
                    new_nodes: Arc::from([]),
                    updated_nodes: Arc::from([]),
                    server_name: Some(self.enricher.name().to_string()),
                    error_count: 0,
                    aborted: false,
                }])
            }
        }
    }

    /// `LspConsumer` runs an external LSP server process — its output depends on
    /// runtime LSP server state beyond the event payload. The bus must not cache
    /// its output; every enrichment must run fresh.
    fn is_cacheable(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// AllEnrichmentsGate
// ---------------------------------------------------------------------------

/// Counts expected `LanguageDetected` events and received `EnrichmentComplete` events.
///
/// When the counts match, emits `AllEnrichmentsDone` with the merged LSP results.
/// This is the synchronisation point that lets `EnrichmentFinalizer` wait for all
/// LSP enrichers to finish before running post-extraction passes.
///
/// **Singleton per bus instance.** One gate serves all languages for a single root.
/// Multiple roots would need separate gate instances (each bus is per-call in
/// `emit_enrichment_pipeline`).
///
/// **Interior mutability via `Mutex`** because `on_event` receives `&self`
/// (the `ExtractionConsumer` trait is `&self` for Send + Sync compatibility).
///
/// Subscribes to: `RootExtracted` (to record base nodes/edges and language count),
///                `EnrichmentComplete` (to accumulate results and detect completion)
/// Emits: `AllEnrichmentsDone` (exactly once, when all enrichments are received)
pub struct AllEnrichmentsGate {
    /// Shared state protected by a Mutex — on_event is &self but needs mutation.
    state: Mutex<GateState>,
    /// When true, LSP consumers are skipped (#574) — force expected=0 so the gate
    /// emits AllEnrichmentsDone immediately on RootExtracted.
    skip_lsp: bool,
}

struct GateState {
    /// Number of `LanguageDetected` events expected (set on `RootExtracted`).
    expected: usize,
    /// Number of `EnrichmentComplete` events received so far.
    received: usize,
    /// Base nodes from `RootExtracted`.
    base_nodes: Option<Arc<[Node]>>,
    /// Base edges from `RootExtracted`.
    base_edges: Option<Arc<[crate::graph::Edge]>>,
    /// Root slug (from `RootExtracted`).
    slug: Option<String>,
    /// Accumulated LSP edges from all `EnrichmentComplete` events.
    lsp_edges: Vec<crate::graph::Edge>,
    /// Accumulated virtual nodes from all `EnrichmentComplete` events.
    lsp_nodes: Vec<Node>,
    /// Accumulated metadata patches from all `EnrichmentComplete` events.
    /// Each patch is `(node_stable_id, key-value map)`.
    updated_nodes: Vec<(String, std::collections::BTreeMap<String, String>)>,
    /// Whether `AllEnrichmentsDone` has already been emitted (guards against double-emit).
    fired: bool,
}

impl AllEnrichmentsGate {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(GateState {
                expected: 0,
                received: 0,
                base_nodes: None,
                base_edges: None,
                slug: None,
                lsp_edges: Vec::new(),
                lsp_nodes: Vec::new(),
                updated_nodes: Vec::new(),
                fired: false,
            }),
            skip_lsp: false,
        }
    }

    /// Create a gate that skips LSP — forces expected=0 so AllEnrichmentsDone
    /// fires immediately. Used when LSP is deferred to background (#574).
    pub fn with_skip_lsp() -> Self {
        Self {
            state: Mutex::new(GateState {
                expected: 0,
                received: 0,
                base_nodes: None,
                base_edges: None,
                slug: None,
                lsp_edges: Vec::new(),
                lsp_nodes: Vec::new(),
                updated_nodes: Vec::new(),
                fired: false,
            }),
            skip_lsp: true,
        }
    }
}

impl Default for AllEnrichmentsGate {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExtractionConsumer for AllEnrichmentsGate {
    fn name(&self) -> &str { "all_enrichments_gate" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootExtracted, ExtractionEventKind::EnrichmentComplete]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let mut state = self.state.lock().expect("AllEnrichmentsGate mutex poisoned");

        match event {
            ExtractionEvent::RootExtracted { slug, nodes, edges, dirty_slugs, .. } => {
                // Only count languages from dirty-root nodes. This must agree with
                // `LanguageAccumulatorConsumer` which only emits `LanguageDetected`
                // for dirty-root nodes.
                // - `None` = all roots dirty
                // - `Some(set)` = only roots in set are dirty
                let dirty_set = dirty_slugs.as_ref();

                // Count distinct languages in the node set to know how many
                // `EnrichmentComplete` events to expect. This must agree with
                // `LanguageAccumulatorConsumer` which emits one `LanguageDetected`
                // per distinct non-empty language from dirty roots.
                let language_count: usize = {
                    let mut seen = std::collections::HashSet::new();
                    for n in nodes.iter() {
                        if !n.language.is_empty() {
                            let is_dirty = match dirty_set {
                                None => true,
                                Some(set) => set.contains(&n.id.root),
                            };
                            if is_dirty {
                                seen.insert(n.language.clone());
                            }
                        }
                    }
                    seen.len()
                };

                // Filter to languages that have a registered LspConsumer (i.e., are
                // supported by EnricherRegistry). Languages without an enricher will
                // never produce EnrichmentComplete, so we must not count them.
                let supported = crate::extract::EnricherRegistry::with_builtins()
                    .supported_languages();
                let expected = if self.skip_lsp {
                    // #574: LSP consumers are not registered — no EnrichmentComplete
                    // events will arrive. Force expected=0 so we emit AllEnrichmentsDone
                    // immediately, allowing non-LSP passes to run without waiting.
                    0
                } else {
                    let mut seen = std::collections::HashSet::new();
                    for n in nodes.iter() {
                        if !n.language.is_empty() && supported.contains(&n.language) {
                            let is_dirty = match dirty_set {
                                None => true,
                                Some(set) => set.contains(&n.id.root),
                            };
                            if is_dirty {
                                seen.insert(n.language.clone());
                            }
                        }
                    }
                    seen.len()
                };

                tracing::debug!(
                    "AllEnrichmentsGate: root '{}' — {} distinct language(s) in dirty-root nodes, \
                     {} supported by EnricherRegistry (dirty_slugs: {:?})",
                    slug,
                    language_count,
                    expected,
                    dirty_slugs,
                );

                state.expected = expected;
                state.received = 0;
                state.base_nodes = Some(Arc::clone(nodes));
                state.base_edges = Some(Arc::clone(edges));
                state.slug = Some(slug.clone());
                state.lsp_edges.clear();
                state.lsp_nodes.clear();
                state.updated_nodes.clear();
                state.fired = false;

                // If there are no supported languages, emit AllEnrichmentsDone immediately
                // so EnrichmentFinalizer doesn't wait forever.
                if expected == 0 {
                    tracing::debug!(
                        "AllEnrichmentsGate: root '{}' — no supported languages, emitting AllEnrichmentsDone immediately",
                        slug,
                    );
                    state.fired = true;
                    return Ok(vec![ExtractionEvent::AllEnrichmentsDone {
                        slug: slug.clone(),
                        nodes: Arc::clone(nodes),
                        edges: Arc::clone(edges),
                        lsp_edges: Arc::from([]),
                        lsp_nodes: Arc::from([]),
                        updated_nodes: Arc::from([]),
                    }]);
                }

                Ok(vec![])
            }

            ExtractionEvent::EnrichmentComplete { slug, added_edges, new_nodes, updated_nodes, .. } => {
                // Guard: only process if we have state initialised for this slug.
                if state.slug.as_deref() != Some(slug.as_str()) {
                    tracing::warn!(
                        "AllEnrichmentsGate: received EnrichmentComplete for '{}' but expected '{:?}'",
                        slug,
                        state.slug,
                    );
                    return Ok(vec![]);
                }
                if state.fired {
                    // Already emitted AllEnrichmentsDone — ignore duplicate completions.
                    return Ok(vec![]);
                }

                state.received += 1;
                state.lsp_edges.extend_from_slice(added_edges);
                state.lsp_nodes.extend_from_slice(new_nodes);
                state.updated_nodes.extend_from_slice(updated_nodes);

                tracing::debug!(
                    "AllEnrichmentsGate: root '{}' — {}/{} enrichments complete",
                    slug,
                    state.received,
                    state.expected,
                );

                if state.received >= state.expected {
                    // All enrichments received — emit AllEnrichmentsDone.
                    state.fired = true;
                    let base_nodes = state.base_nodes.clone()
                        .unwrap_or_else(|| Arc::from([]));
                    let base_edges = state.base_edges.clone()
                        .unwrap_or_else(|| Arc::from([]));
                    let lsp_edges: Arc<[crate::graph::Edge]> = Arc::from(
                        std::mem::take(&mut state.lsp_edges).into_boxed_slice()
                    );
                    let lsp_nodes: Arc<[Node]> = Arc::from(
                        std::mem::take(&mut state.lsp_nodes).into_boxed_slice()
                    );
                    let updated_nodes: Arc<[(String, std::collections::BTreeMap<String, String>)]> = Arc::from(
                        std::mem::take(&mut state.updated_nodes).into_boxed_slice()
                    );
                    tracing::info!(
                        "AllEnrichmentsGate: root '{}' — all {} enrichment(s) done, \
                         {} LSP edges, {} LSP nodes, {} metadata patches",
                        slug,
                        state.expected,
                        lsp_edges.len(),
                        lsp_nodes.len(),
                        updated_nodes.len(),
                    );
                    return Ok(vec![ExtractionEvent::AllEnrichmentsDone {
                        slug: slug.clone(),
                        nodes: base_nodes,
                        edges: base_edges,
                        lsp_edges,
                        lsp_nodes,
                        updated_nodes,
                    }]);
                }

                Ok(vec![])
            }

            _ => Ok(vec![]),
        }
    }

    /// `AllEnrichmentsGate` is stateful — it accumulates `EnrichmentComplete` events
    /// across multiple `on_event` calls before emitting `AllEnrichmentsDone`.
    /// The bus must not cache its output; every call must reach it.
    fn is_cacheable(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// EmbeddingIndexerConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `RootExtracted` and streams nodes to the embedding index.
///
/// Per the ADR: "Consumer of RootExtracted (streaming, as nodes arrive):
/// EmbeddingIndexer — SINGLETON — embeds nodes as they're extracted incrementally
/// via BLAKE3, doesn't wait for passes."
///
/// When constructed with `Some(idx)`, fires a background tokio task to call
/// `idx.reindex_nodes(nodes)` for each `RootExtracted` event. External nodes
/// (root == "external") are filtered out — they have no source text to embed.
///
/// When constructed with `None` (stub mode, used in tests), logs a debug
/// message and emits nothing — identical to the Phase 2 stub behaviour.
///
/// Subscribes to: `RootExtracted`
/// Emits: nothing
pub struct EmbeddingIndexerConsumer {
    /// Shared embedding index. `None` in stub/test mode; `Some` in production.
    pub idx: Option<Arc<crate::embed::EmbeddingIndex>>,
}

impl EmbeddingIndexerConsumer {
    /// Create a real consumer that will embed nodes as they arrive.
    pub fn new(idx: Arc<crate::embed::EmbeddingIndex>) -> Self {
        Self { idx: Some(idx) }
    }

    /// Create a stub consumer that does nothing (for tests and Phase 2 callers).
    pub fn stub() -> Self {
        Self { idx: None }
    }
}

#[async_trait]
impl ExtractionConsumer for EmbeddingIndexerConsumer {
    fn name(&self) -> &str { "embedding_indexer" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootExtracted]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::RootExtracted { slug, nodes, .. } = event else {
            return Ok(vec![]);
        };

        let Some(ref idx) = self.idx else {
            tracing::debug!(
                "EmbeddingIndexerConsumer: root '{}' — {} nodes (stub mode, embed deferred)",
                slug,
                nodes.len(),
            );
            return Ok(vec![]);
        };

        // Filter external/virtual nodes — they have no source text to embed.
        let embeddable: Vec<Node> = nodes.iter()
            .filter(|n| n.id.root != "external")
            .cloned()
            .collect();

        if embeddable.is_empty() {
            tracing::debug!(
                "EmbeddingIndexerConsumer: root '{}' — no embeddable nodes",
                slug,
            );
            return Ok(vec![]);
        }

        let idx_clone = Arc::clone(idx);
        let slug_clone = slug.clone();
        let count = embeddable.len();

        // Spawn a background task. Uses `Handle::try_current()` so this is safe
        // to call from both async contexts (MCP server) and sync tests using a
        // current_thread runtime.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    match idx_clone.reindex_nodes(&embeddable).await {
                        Ok(n) => tracing::info!(
                            "EmbeddingIndexerConsumer: root '{}' — embedded {} / {} nodes",
                            slug_clone, n, count,
                        ),
                        Err(e) => tracing::warn!(
                            "EmbeddingIndexerConsumer: root '{}' — embed failed: {}",
                            slug_clone, e,
                        ),
                    }
                });
            }
            Err(_) => {
                // No async runtime available (e.g., purely sync test). Log and skip.
                tracing::debug!(
                    "EmbeddingIndexerConsumer: root '{}' — no async runtime, skipping embed of {} nodes",
                    slug, count,
                );
            }
        }

        Ok(vec![])
    }

    /// `EmbeddingIndexerConsumer` triggers external side-effects (spawns async embed tasks).
    /// It must not be cached — the embed task must fire on every `RootExtracted` event.
    fn is_cacheable(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// LanceDBConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `PassesComplete` and persists the graph to LanceDB.
///
/// Per the ADR: "Consumer of PassesComplete: LanceDBPersist — SINGLETON —
/// writes with tenant_id = root slug."
///
/// When constructed with `Some(repo_root)`, fires a background tokio task to
/// call `persist_graph_to_lance(repo_root, nodes, edges)` after all post-
/// extraction passes have run.
///
/// When constructed with `None` (stub mode, used in tests), logs a debug
/// message and emits nothing — preserving Phase 2 stub behaviour.
///
/// Note: the direct `persist_graph_to_lance` call in `build_full_graph_inner`
/// is retained for ordering with sentinel writes and the `lance_write_lock`.
/// This consumer provides a secondary/background persist path that fires as
/// soon as `PassesComplete` is emitted during the bus run.
///
/// Subscribes to: `PassesComplete`
/// Emits: nothing
pub struct LanceDBConsumer {
    /// Repository root for LanceDB path resolution. `None` in stub/test mode.
    pub repo_root: Option<Arc<PathBuf>>,
}

impl LanceDBConsumer {
    /// Create a real consumer that will persist the graph on `PassesComplete`.
    pub fn new(repo_root: Arc<PathBuf>) -> Self {
        Self { repo_root: Some(repo_root) }
    }

    /// Create a stub consumer that does nothing (for tests and Phase 2 callers).
    pub fn stub() -> Self {
        Self { repo_root: None }
    }
}

#[async_trait]
impl ExtractionConsumer for LanceDBConsumer {
    fn name(&self) -> &str { "lancedb_persist" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::PassesComplete]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::PassesComplete { slug, nodes, edges, .. } = event else {
            return Ok(vec![]);
        };

        let Some(ref repo_root) = self.repo_root else {
            tracing::debug!(
                "LanceDBConsumer: root '{}' — {} nodes, {} edges (stub mode, persist deferred)",
                slug,
                nodes.len(),
                edges.len(),
            );
            return Ok(vec![]);
        };

        let repo_root_clone = Arc::clone(repo_root);
        let nodes_vec: Vec<crate::graph::Node> = nodes.to_vec();
        let edges_vec: Vec<crate::graph::Edge> = edges.to_vec();
        let slug_clone = slug.clone();

        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    match crate::server::store::persist_graph_to_lance(
                        &repo_root_clone,
                        &nodes_vec,
                        &edges_vec,
                    ).await {
                        Ok(()) => tracing::info!(
                            "LanceDBConsumer: root '{}' — persisted {} nodes, {} edges",
                            slug_clone, nodes_vec.len(), edges_vec.len(),
                        ),
                        Err(e) => tracing::warn!(
                            "LanceDBConsumer: root '{}' — persist failed: {}",
                            slug_clone, e,
                        ),
                    }
                });
            }
            Err(_) => {
                tracing::debug!(
                    "LanceDBConsumer: root '{}' — no async runtime, skipping persist of {} nodes, {} edges",
                    slug,
                    nodes.len(),
                    edges.len(),
                );
            }
        }

        Ok(vec![])
    }

    /// `LanceDBConsumer` triggers external side-effects (spawns async LanceDB persist tasks).
    /// It must not be cached — persistence must happen on every `PassesComplete` event.
    fn is_cacheable(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// SubsystemConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `CommunityDetectionComplete` and runs subsystem node promotion.
///
/// Reacts to the event fired by `graph.rs` after PageRank and Louvain community
/// detection complete. Runs:
///   1. `subsystem_node_pass` — promotes detected communities to first-class
///      `NodeKind::Other("subsystem")` nodes with `BelongsTo` edges.
///   2. `subsystem_framework_aggregation_pass` — emits `UsesFramework` edges from
///      subsystem nodes to framework nodes when ≥70% of members share a framework.
///
/// Returns `SubsystemNodesComplete` carrying only the newly added nodes/edges so
/// that `graph.rs` can extend its working collections without duplicating the full set.
///
/// Subscribes to: `CommunityDetectionComplete`
/// Emits: `SubsystemNodesComplete`
pub struct SubsystemConsumer;

#[async_trait]
impl ExtractionConsumer for SubsystemConsumer {
    fn name(&self) -> &str { "subsystem" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::CommunityDetectionComplete]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::CommunityDetectionComplete { slug, subsystems, nodes } = event else {
            return Ok(vec![]);
        };

        // Run subsystem node promotion pass.
        let sub_result = crate::extract::subsystem_pass::subsystem_node_pass(subsystems, nodes, slug);

        // Build the combined node set (existing + new subsystem nodes) needed for
        // framework aggregation, which looks for subsystem nodes by kind.
        let combined_nodes: Vec<crate::graph::Node> = nodes.iter()
            .cloned()
            .chain(sub_result.nodes.iter().cloned())
            .collect();

        // Run subsystem → framework aggregation pass.
        let fw_edges = crate::extract::framework_detection::subsystem_framework_aggregation_pass(&combined_nodes);

        let added_node_count = sub_result.nodes.len();
        let added_edge_count = sub_result.edges.len() + fw_edges.len();

        if added_node_count > 0 {
            tracing::info!(
                "SubsystemConsumer: root '{}' — promoted {} subsystem(s) to first-class nodes",
                slug,
                added_node_count,
            );
        }
        if !fw_edges.is_empty() {
            tracing::info!(
                "SubsystemConsumer: root '{}' — subsystem-framework aggregation: {} UsesFramework edge(s)",
                slug,
                fw_edges.len(),
            );
        }
        tracing::debug!(
            "SubsystemConsumer: root '{}' — {} new nodes, {} new edges",
            slug,
            added_node_count,
            added_edge_count,
        );

        let mut all_added_edges = sub_result.edges;
        all_added_edges.extend(fw_edges);

        Ok(vec![ExtractionEvent::SubsystemNodesComplete {
            slug: slug.clone(),
            added_nodes: Arc::from(sub_result.nodes.into_boxed_slice()),
            added_edges: Arc::from(all_added_edges.into_boxed_slice()),
        }])
    }

    /// `SubsystemConsumer` is a pure transformation (same subsystems → same output)
    /// so caching is appropriate.
    fn is_cacheable(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// EventBus::with_builtins
// ---------------------------------------------------------------------------

/// Optional dependency overrides for [`build_builtin_bus`] and [`emit_enrichment_pipeline`].
///
/// Groups the three optional parameters that control which consumers are wired
/// in real vs stub mode. All fields default to `None` (stub / throwaway).
#[derive(Default)]
pub struct BusOptions {
    /// Shared `ScanStats` handle owned by `RnaHandler`. When `None`, a fresh
    /// throwaway `Arc` is created. Only pass `Some` from server call sites.
    pub scan_stats: Option<Arc<RwLock<ScanStats>>>,
    /// When `Some`, `EmbeddingIndexerConsumer` streams embed tasks per `RootExtracted`.
    pub embed_idx: Option<Arc<crate::embed::EmbeddingIndex>>,
    /// When `Some`, `LanceDBConsumer` fires a background persist on `PassesComplete`.
    pub lance_repo_root: Option<Arc<PathBuf>>,
    /// When `true`, `LspConsumer` instances are NOT registered in the bus.
    /// This allows the enrichment pipeline to run non-LSP passes only (fast path),
    /// with LSP enrichment deferred to a background task (#574).
    pub skip_lsp: bool,
}

/// Build an `EventBus` pre-loaded with all built-in consumers.
///
/// **Phase 3 wiring**: `LspConsumer` instances are now real — they hold
/// `Arc<dyn Enricher>` and run actual LSP enrichment in `on_event`.
/// `AllEnrichmentsGate` gates `EnrichmentFinalizer` so passes run only after
/// all LSP enrichers have completed. The `spawn_background_enrichment` LSP path
/// in `build_full_graph_inner` is no longer needed.
///
/// Registered consumers:
/// - `ScanStatsConsumer` — `RootDiscovered`, `RootExtracted`, `LanguageDetected`,
///   `EnrichmentComplete`, `PassesComplete` (singleton; writes into `scan_stats`)
/// - `ManifestConsumer`, `TreeSitterConsumer` — `RootDiscovered`
/// - `LanguageAccumulatorConsumer` — `RootExtracted` → `LanguageDetected`
/// - `LspConsumer(lang)` × N — `LanguageDetected` → `EnrichmentComplete`
/// - `AllEnrichmentsGate` — `RootExtracted` (count) + `EnrichmentComplete` → `AllEnrichmentsDone`
/// - `ApiLinkConsumer`, `TestedByConsumer` — `AllEnrichmentsDone` (monitoring, `PassComplete`)
/// - `EnrichmentFinalizer` — `AllEnrichmentsDone` → `PassesComplete` + `FrameworkDetected`
/// - `OpenApiConsumer`, `GrpcConsumer`, `EmbeddingIndexerConsumer` — `RootExtracted` (side-effects)
/// - `FrameworkDetectionConsumer`, `FastapiRouterPrefixConsumer`, `SdkPathInferenceConsumer`,
///   `NextjsRoutingConsumer`, `PubSubConsumer`, `WebSocketConsumer` — `FrameworkDetected`
/// - `LanceDBConsumer` — `PassesComplete`
/// - `SubsystemConsumer` — `CommunityDetectionComplete` → `SubsystemNodesComplete`
///
/// Note: `CustomExtractorConsumer` is not pre-registered here because the set of
/// custom extractors is config-driven (`.oh/extractors/*.toml`) and unknown at
/// startup. Callers loading custom extractor configs must register additional
/// `CustomExtractorConsumer` instances after calling `build_builtin_bus()`.
///
/// `root_pairs` must be the full `(slug, path)` list for the workspace.
/// `primary_slug` is the slug of the primary code root.
/// `repo_root` is the path to the primary repository root (for LSP server startup).
/// `opts` bundles the optional dependency overrides; see [`BusOptions`].
///
/// Returns `(bus, scan_stats_arc)` where `scan_stats_arc` is either the passed-in
/// `Arc` or the freshly created one.
pub fn build_builtin_bus(
    root_pairs: Vec<(String, PathBuf)>,
    primary_slug: String,
    repo_root: PathBuf,
    opts: BusOptions,
) -> (crate::extract::event_bus::EventBus, Arc<RwLock<ScanStats>>) {
    let BusOptions { scan_stats, embed_idx, lance_repo_root, skip_lsp } = opts;
    use crate::extract::event_bus::EventBus;

    let mut bus = EventBus::new();

    // --- ScanStatsConsumer (singleton — registered first so it sees every event) ---
    let stats_arc = scan_stats.unwrap_or_else(|| Arc::new(RwLock::new(ScanStats::default())));
    let scan_stats_consumer = ScanStatsConsumer { stats: Arc::clone(&stats_arc) };
    bus.register(Box::new(scan_stats_consumer));

    // --- RootDiscovered consumers ---
    bus.register(Box::new(ManifestConsumer));
    bus.register(Box::new(TreeSitterConsumer::with_scan_stats(Arc::clone(&stats_arc))));

    // --- RootExtracted consumers ---
    // LanguageAccumulatorConsumer must run first (emits LanguageDetected which
    // triggers LspConsumers, then AllEnrichmentsGate counts up).
    bus.register(Box::new(LanguageAccumulatorConsumer));

    // AllEnrichmentsGate: must subscribe to RootExtracted BEFORE LspConsumers
    // fire so it captures the language count before any EnrichmentComplete arrives.
    // Registration order matches subscription order in the sync bus.
    // When skip_lsp=true (#574), the gate forces expected=0 so AllEnrichmentsDone
    // fires immediately without waiting for EnrichmentComplete events.
    bus.register(Box::new(if skip_lsp {
        AllEnrichmentsGate::with_skip_lsp()
    } else {
        AllEnrichmentsGate::new()
    }));

    // OpenApi, gRPC, Embedding — subscribe to RootExtracted independently.
    bus.register(Box::new(OpenApiConsumer));
    bus.register(Box::new(GrpcConsumer));
    // EmbeddingIndexerConsumer: streaming embed as nodes arrive.
    // Passes None in stub/test mode; Some(idx) in production for real streaming embed.
    bus.register(Box::new(match embed_idx {
        Some(idx) => EmbeddingIndexerConsumer::new(idx),
        None => EmbeddingIndexerConsumer::stub(),
    }));

    // --- LanguageDetected consumers (one real LspConsumer per language) ---
    // Per the ADR: "ALL fire concurrently, one per language."
    // Currently sequential in the sync bus; Phase 4 promotes to async parallel.
    //
    // When `skip_lsp=true` (#574), LspConsumers are omitted entirely so the bus
    // completes without waiting for LSP servers. The caller spawns LSP enrichment
    // in background and ArcSwaps the enriched graph when it finishes.
    if !skip_lsp {
        // Build the enricher registry once and extract individual enrichers per language.
        // Each `LspConsumer` owns an `Arc<dyn Enricher>` so it doesn't re-instantiate.
        let enricher_registry = crate::extract::EnricherRegistry::with_builtins();
        let lsp_roots: Arc<Vec<(String, PathBuf)>> = Arc::new(root_pairs.clone());

        // Build enrichers indexed by language for O(1) lookup.
        // EnricherRegistry does not expose individual enrichers, so we rebuild
        // per-language enrichers via LspEnricher::new (same as EnricherRegistry internals).
        // This avoids exposing registry internals and keeps consumers self-contained.
        let mut supported_languages: Vec<String> = enricher_registry
            .supported_languages()
            .into_iter()
            .collect();
        supported_languages.sort(); // deterministic registration order

        for lang in &supported_languages {
            // Build a single-language enricher for this consumer.
            // The enricher is the same type as what EnricherRegistry uses internally.
            // We use `EnricherRegistry::with_builtins()` filtered to this language
            // rather than duplicating the language-server config table.
            let single_lang_enricher = build_single_language_enricher(lang);
            bus.register(Box::new(LspConsumer {
                language: lang.clone(),
                enricher: single_lang_enricher,
                repo_root: repo_root.clone(),
                lsp_roots: Arc::clone(&lsp_roots),
            }));
        }
    } else {
        tracing::info!("skip_lsp=true: LspConsumers omitted from bus — LSP will run in background");
    }

    // --- AllEnrichmentsDone consumers ---
    // ApiLinkConsumer and TestedByConsumer: subscribe for monitoring/PassComplete signals.
    // EnrichmentFinalizer: the authoritative pass orchestrator — always emits PassesComplete.
    bus.register(Box::new(ApiLinkConsumer));
    bus.register(Box::new(TestedByConsumer));
    bus.register(Box::new(EnrichmentFinalizer::new(root_pairs.clone(), primary_slug)));

    // --- FrameworkDetected consumers ---
    bus.register(Box::new(FrameworkDetectionConsumer));
    bus.register(Box::new(FastapiRouterPrefixConsumer));
    bus.register(Box::new(SdkPathInferenceConsumer));
    bus.register(Box::new(NextjsRoutingConsumer::new(root_pairs)));
    bus.register(Box::new(PubSubConsumer));
    bus.register(Box::new(WebSocketConsumer));

    // --- PassesComplete consumers ---
    // LanceDBConsumer: persist graph on PassesComplete.
    // Passes None in stub/test mode; Some(repo_root) in production.
    bus.register(Box::new(match lance_repo_root {
        Some(rr) => LanceDBConsumer::new(rr),
        None => LanceDBConsumer::stub(),
    }));

    // --- CommunityDetectionComplete consumers ---
    // SubsystemConsumer: runs subsystem_node_pass + subsystem_framework_aggregation_pass
    // after PageRank and community detection complete. Fired by graph.rs, not the
    // extraction pipeline, so this registration slot is "always present" and the bus
    // routes CommunityDetectionComplete → SubsystemConsumer → SubsystemNodesComplete.
    bus.register(Box::new(SubsystemConsumer));

    (bus, stats_arc)
}

/// Build a single-language `Arc<dyn Enricher>` for use in `LspConsumer`.
///
/// Constructs an `LspEnricher` with the same configuration as `EnricherRegistry::with_builtins()`
/// for the given language. Returns a no-op enricher if the language is not recognised.
fn build_single_language_enricher(language: &str) -> Arc<dyn crate::extract::Enricher> {
    use crate::extract::lsp::LspEnricher;

    // Mirror the server table from EnricherRegistry::with_builtins().
    // Keep this in sync with that table — a test in consumers::tests verifies parity.
    let servers: &[(&str, &str, &[&str], &[&str])] = &[
        ("rust",         "rust-analyzer",              &[],         &["rs"]),
        ("python",       "pyright-langserver",         &["--stdio"], &["py"]),
        ("typescript",   "typescript-language-server",  &["--stdio"], &["ts", "tsx", "js", "jsx"]),
        ("go",           "gopls",                      &["serve"],  &["go"]),
        ("markdown",     "marksman",                   &["server"], &["md"]),
        ("c-cpp",        "clangd",                     &[],         &["c", "cc", "cpp", "cxx", "h", "hpp"]),
        ("java",         "jdtls",                      &[],         &["java"]),
        ("ruby",         "solargraph",                 &["stdio"],  &["rb"]),
        ("csharp",       "omnisharp",                  &["-lsp"],   &["cs"]),
        ("swift",        "sourcekit-lsp",              &[],         &["swift"]),
        ("kotlin",       "kotlin-language-server",     &[],         &["kt", "kts"]),
        ("lua",          "lua-language-server",        &[],         &["lua"]),
        ("zig",          "zls",                        &[],         &["zig"]),
        ("elixir",       "elixir-ls",                  &[],         &["ex", "exs"]),
        ("haskell",      "haskell-language-server",    &["--lsp"],  &["hs"]),
        ("ocaml",        "ocamllsp",                   &[],         &["ml", "mli"]),
        ("scala",        "metals",                     &[],         &["scala", "sc"]),
        ("dart",         "dart",                       &["language-server"], &["dart"]),
        ("r",            "R",                          &["--no-echo", "-e", "languageserver::run()"], &["r", "R"]),
        ("julia",        "julia",                      &["--startup-file=no", "-e", "using LanguageServer; runserver()"], &["jl"]),
        ("php",          "intelephense",               &["--stdio"], &["php"]),
        ("css",          "vscode-css-languageserver",  &["--stdio"], &["css", "scss", "less"]),
        ("html",         "vscode-html-languageserver", &["--stdio"], &["html", "htm"]),
        ("yaml",         "yaml-language-server",       &["--stdio"], &["yaml", "yml"]),
        ("json",         "vscode-json-languageserver", &["--stdio"], &["json"]),
        ("toml",         "taplo",                      &["lsp", "stdio"], &["toml"]),
        ("terraform",    "terraform-ls",               &["serve"],  &["tf", "tfvars"]),
        ("nix",          "nil",                        &[],         &["nix"]),
        ("vue",          "vue-language-server",        &["--stdio"], &["vue"]),
        ("svelte",       "svelteserver",               &["--stdio"], &["svelte"]),
        ("erlang",       "erlang_ls",                  &[],         &["erl", "hrl"]),
        ("gleam",        "gleam",                      &["lsp"],    &["gleam"]),
        ("nim",          "nimlsp",                     &[],         &["nim"]),
        ("clojure",      "clojure-lsp",               &[],         &["clj", "cljs", "cljc"]),
        ("deno",         "deno",                       &["lsp"],    &["ts", "tsx", "js", "jsx"]),
        ("protobuf",     "buf",                        &["lsp"],    &["proto"]),
        ("latex",        "texlab",                     &[],         &["tex", "bib"]),
        ("typst",        "tinymist",                   &[],         &["typ"]),
    ];

    for &(lang, cmd, args, exts) in servers {
        if lang == language {
            let enricher = LspEnricher::new(lang, cmd, args, exts);
            let enricher = if lang == "python" {
                enricher.with_settings(serde_json::json!({
                    "python": { "analysis": { "autoSearchPaths": true } }
                }))
            } else {
                enricher
            };
            let enricher = match lang {
                "typescript" | "deno" => enricher.with_config_file("tsconfig.json"),
                "python"              => enricher.with_config_file("pyproject.toml"),
                "go"                  => enricher.with_config_file("go.mod"),
                "rust"                => enricher.with_config_file("Cargo.toml"),
                "java"                => enricher.with_config_file("pom.xml"),
                "kotlin"              => enricher.with_config_file("build.gradle.kts"),
                _                     => enricher,
            };
            return Arc::new(enricher);
        }
    }

    // Fallback: no-op enricher for unrecognised languages.
    // This should never happen if `supported_languages` is derived from the same table.
    tracing::warn!("build_single_language_enricher: unrecognised language '{}' — using no-op", language);
    Arc::new(NoopEnricher { language: language.to_string() })
}

/// No-op enricher used as a fallback when a language is not in the server table.
struct NoopEnricher {
    language: String,
}

#[async_trait::async_trait]
impl crate::extract::Enricher for NoopEnricher {
    fn languages(&self) -> &[&str] { &[] }
    fn is_ready(&self) -> bool { false }
    async fn enrich(
        &self,
        _nodes: &[Node],
        _index: &crate::graph::index::GraphIndex,
        _repo_root: &std::path::Path,
    ) -> anyhow::Result<crate::extract::EnrichmentResult> {
        Ok(crate::extract::EnrichmentResult::default())
    }
    fn name(&self) -> &str { &self.language }
}

// ---------------------------------------------------------------------------
// emit_enrichment_pipeline
// ---------------------------------------------------------------------------

/// Emit a `RootExtracted` event through a pre-built bus and collect the resulting
/// `PassesComplete` data.
///
/// This is the common pattern used by `build_full_graph_inner`,
/// `update_graph_with_scan`, and the background scanner:
///
/// ```text
/// nodes + edges → Arc<[T]> → RootExtracted → bus → LanguageAccumulatorConsumer
///     → LspConsumer × N → AllEnrichmentsGate → EnrichmentFinalizer
///     → PassesComplete { nodes, edges, detected_frameworks }
/// ```
///
/// The `nodes` and `edges` arguments are consumed (moved into `Arc<[T]>` for
/// zero-copy fan-out to the bus consumers).
///
/// # Arguments
/// * `nodes` - extracted node set (consumed)
/// * `edges` - extracted edge set (consumed)
/// * `root_pairs` - `(slug, path)` pairs for all workspace roots
/// * `primary_slug` - slug of the primary code root
/// * `repo_root` - path to the primary repository root (for the bus event)
/// * `opts` - optional dependency overrides; see [`BusOptions`]. Pass
///   `BusOptions::default()` for stub/test mode (all fields `None`).
///
/// # Returns
/// `Ok((nodes, edges, detected_frameworks))` — the enriched graph and framework set.
///
/// # Errors
/// Returns `Err` if the bus does not produce a `PassesComplete` event.
/// This is a pipeline invariant violation (`EnrichmentFinalizer` always emits
/// `PassesComplete`), so `Err` here indicates a logic bug, not a transient error.
pub async fn emit_enrichment_pipeline(
    nodes: Vec<crate::graph::Node>,
    edges: Vec<crate::graph::Edge>,
    root_pairs: Vec<(String, PathBuf)>,
    primary_slug: String,
    repo_root: PathBuf,
    opts: BusOptions,
    dirty_slugs: Option<std::collections::HashSet<String>>,
) -> anyhow::Result<(Vec<crate::graph::Node>, Vec<crate::graph::Edge>, std::collections::HashSet<String>)> {
    use crate::extract::event_bus::ExtractionEvent;

    // Wrap into Arc<[T]> for zero-copy bus fan-out.
    let nodes_arc: Arc<[crate::graph::Node]> = Arc::from(nodes.into_boxed_slice());
    let edges_arc: Arc<[crate::graph::Edge]> = Arc::from(edges.into_boxed_slice());

    let (mut bus, _stats) = build_builtin_bus(root_pairs, primary_slug.clone(), repo_root.clone(), opts);
    let events = bus.emit(ExtractionEvent::RootExtracted {
        slug: primary_slug,
        path: repo_root,
        nodes: Arc::clone(&nodes_arc),
        edges: Arc::clone(&edges_arc),
        dirty_slugs,
    }).await;

    // Collect PassesComplete — produced by EnrichmentFinalizer.
    // PassesComplete is a pipeline invariant: EnrichmentFinalizer always emits it.
    // If it's absent, the bus was misconfigured or a consumer panicked — treat as error.
    let passes_complete = events.into_iter().find(|e| {
        matches!(e, ExtractionEvent::PassesComplete { .. })
    });

    match passes_complete {
        Some(ExtractionEvent::PassesComplete {
            nodes,
            edges,
            detected_frameworks,
            ..
        }) => {
            Ok((nodes.to_vec(), edges.to_vec(), detected_frameworks))
        }
        _ => {
            anyhow::bail!(
                "EventBus enrichment pipeline: PassesComplete event absent — \
                 this is a pipeline invariant violation (EnrichmentFinalizer \
                 must always emit PassesComplete). Failing hard to prevent \
                 persisting an unenriched graph."
            )
        }
    }
}

// ---------------------------------------------------------------------------
// emit_community_detection
// ---------------------------------------------------------------------------

/// Emit a `CommunityDetectionComplete` event through a slim bus (SubsystemConsumer only)
/// and return the resulting `(added_nodes, added_edges)`.
///
/// This is the public API that `graph.rs` calls after PageRank and community detection
/// complete. It fully replaces the direct `subsystem_node_pass` and
/// `subsystem_framework_aggregation_pass` call sites in `graph.rs`.
///
/// Returns `(Vec<Node>, Vec<Edge>)` — the **new** subsystem nodes and edges to extend
/// the working collections with. Returns empty vecs if no subsystems were detected.
///
/// # Errors
/// Returns `Err` only if `SubsystemConsumer::on_event` itself fails, which should
/// not happen in practice (the function is infallible for well-formed inputs).
pub async fn emit_community_detection(
    slug: String,
    subsystems: Vec<crate::graph::index::Subsystem>,
    nodes: Vec<crate::graph::Node>,
) -> anyhow::Result<(Vec<crate::graph::Node>, Vec<crate::graph::Edge>)> {
    use crate::extract::event_bus::{EventBus, ExtractionEvent};

    let mut bus = EventBus::new();
    bus.register(Box::new(SubsystemConsumer));

    let events = bus.emit(ExtractionEvent::CommunityDetectionComplete {
        slug,
        subsystems: Arc::from(subsystems.into_boxed_slice()),
        nodes: Arc::from(nodes.into_boxed_slice()),
    }).await;

    // Collect SubsystemNodesComplete — emitted by SubsystemConsumer.
    let sub_complete = events.into_iter().find(|e| {
        matches!(e, ExtractionEvent::SubsystemNodesComplete { .. })
    });

    match sub_complete {
        Some(ExtractionEvent::SubsystemNodesComplete { added_nodes, added_edges, .. }) => {
            Ok((added_nodes.to_vec(), added_edges.to_vec()))
        }
        _ => {
            // SubsystemConsumer always emits SubsystemNodesComplete (even when empty).
            // If it's absent, the bus was misconfigured. Return empty rather than failing
            // hard — subsystem nodes are a non-critical enrichment.
            tracing::warn!(
                "emit_community_detection: SubsystemNodesComplete event absent — \
                 subsystem promotion may have been skipped"
            );
            Ok((vec![], vec![]))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::event_bus::{ExtractionEvent, ExtractionEventKind};
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Verify ManifestConsumer subscribes to RootDiscovered and emits nothing.
    #[tokio::test]
    async fn test_manifest_consumer_subscription() {
        let consumer = ManifestConsumer;
        assert!(consumer.subscribes_to().contains(&ExtractionEventKind::RootDiscovered));
        let event = ExtractionEvent::RootDiscovered {
            slug: "test".into(),
            path: PathBuf::from("."),
            lsp_only: false,
        };
        let result = consumer.on_event(&event).await.unwrap();
        assert!(result.is_empty(), "ManifestConsumer emits no follow-on events in Phase 2");
    }

    /// Verify LanguageAccumulatorConsumer groups nodes by language.
    #[tokio::test]
    async fn test_language_accumulator_groups_by_language() {
        use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};
        use std::collections::BTreeMap;

        fn make_node(lang: &str, name: &str) -> Node {
            Node {
                id: NodeId {
                    root: "test".into(),
                    file: PathBuf::from(format!("src/{}.{}", name, lang)),
                    name: name.into(),
                    kind: NodeKind::Function,
                },
                language: lang.into(),
                line_start: 1,
                line_end: 1,
                signature: name.into(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            }
        }

        let event = ExtractionEvent::RootExtracted {
            slug: "test".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from(vec![
                make_node("rust", "foo"),
                make_node("rust", "bar"),
                make_node("python", "baz"),
            ].into_boxed_slice()),
            edges: std::sync::Arc::from([]),
            dirty_slugs: None,
        };

        let consumer = LanguageAccumulatorConsumer;
        let follow_ons = consumer.on_event(&event).await.unwrap();

        // Should emit LanguageDetected for "rust" and "python"
        assert_eq!(follow_ons.len(), 2, "Should emit one LanguageDetected per language");
        let langs: HashSet<String> = follow_ons.iter().filter_map(|e| {
            if let ExtractionEvent::LanguageDetected { language, .. } = e {
                Some(language.clone())
            } else {
                None
            }
        }).collect();
        assert!(langs.contains("rust"));
        assert!(langs.contains("python"));
    }

    /// Verify LanguageAccumulatorConsumer filters out nodes from clean roots
    /// when dirty_slugs is non-empty.
    #[tokio::test]
    async fn test_language_accumulator_filters_clean_roots() {
        use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};
        use std::collections::BTreeMap;

        fn make_node_with_root(root: &str, lang: &str, name: &str) -> Node {
            Node {
                id: NodeId {
                    root: root.into(),
                    file: PathBuf::from("test.rs"),
                    name: name.into(),
                    kind: NodeKind::Function,
                },
                language: lang.into(),
                line_start: 1,
                line_end: 10,
                signature: name.into(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            }
        }

        // Two roots: "dirty_root" (dirty) and "clean_root" (not in dirty_slugs).
        // Both have Rust nodes, but only dirty_root's nodes should trigger LanguageDetected.
        let event = ExtractionEvent::RootExtracted {
            slug: "test".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from(vec![
                make_node_with_root("dirty_root", "rust", "foo"),
                make_node_with_root("dirty_root", "rust", "bar"),
                make_node_with_root("clean_root", "rust", "baz"),
                make_node_with_root("clean_root", "python", "qux"),
            ].into_boxed_slice()),
            edges: std::sync::Arc::from([]),
            dirty_slugs: Some(std::collections::HashSet::from(["dirty_root".to_string()])),
        };

        let consumer = LanguageAccumulatorConsumer;
        let follow_ons = consumer.on_event(&event).await.unwrap();

        // Should only emit LanguageDetected for "rust" (from dirty_root).
        // "python" from clean_root should be excluded.
        assert_eq!(follow_ons.len(), 1, "Only dirty-root languages should trigger LanguageDetected");
        if let ExtractionEvent::LanguageDetected { language, nodes, .. } = &follow_ons[0] {
            assert_eq!(language, "rust");
            assert_eq!(nodes.len(), 2, "Only dirty_root rust nodes should be included");
        } else {
            panic!("Expected LanguageDetected event");
        }
    }

    /// Verify LanguageAccumulatorConsumer skips nodes with empty language.
    #[tokio::test]
    async fn test_language_accumulator_skips_empty_language() {
        use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};
        use std::collections::BTreeMap;

        let event = ExtractionEvent::RootExtracted {
            slug: "test".into(),
            path: PathBuf::from("."),
            dirty_slugs: None,
            nodes: std::sync::Arc::from(vec![Node {
                id: NodeId {
                    root: "test".into(),
                    file: PathBuf::from("unknown"),
                    name: "x".into(),
                    kind: NodeKind::Other("unknown".into()),
                },
                language: "".into(), // empty language
                line_start: 1,
                line_end: 1,
                signature: "x".into(),
                body: String::new(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            }].into_boxed_slice()),
            edges: std::sync::Arc::from([]),
        };
        let consumer = LanguageAccumulatorConsumer;
        let follow_ons = consumer.on_event(&event).await.unwrap();
        assert!(follow_ons.is_empty(), "Empty language nodes must be skipped");
    }

    /// Verify TreeSitterConsumer skips lsp_only roots.
    #[tokio::test]
    async fn test_tree_sitter_consumer_skips_lsp_only() {
        let consumer = TreeSitterConsumer::new();
        let event = ExtractionEvent::RootDiscovered {
            slug: "skills".into(),
            path: PathBuf::from("."),
            lsp_only: true,
        };
        let result = consumer.on_event(&event).await.unwrap();
        assert!(result.is_empty(), "lsp_only roots must produce no RootExtracted event");
    }

    /// Verify PubSubConsumer only fires for broker frameworks.
    #[tokio::test]
    async fn test_pubsub_consumer_fires_for_kafka() {
        let consumer = PubSubConsumer;
        let event = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "kafka-python".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).await.unwrap();
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], ExtractionEvent::PassComplete { pass_name: "pubsub", .. }));
    }

    #[tokio::test]
    async fn test_pubsub_consumer_ignores_non_broker() {
        let consumer = PubSubConsumer;
        let event = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "django".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).await.unwrap();
        assert!(result.is_empty());
    }

    /// Verify WebSocketConsumer fires only for socketio.
    #[tokio::test]
    async fn test_websocket_consumer_fires_for_socketio() {
        let consumer = WebSocketConsumer;
        let event = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "socketio".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).await.unwrap();
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], ExtractionEvent::PassComplete { pass_name: "websocket", .. }));
    }

    #[tokio::test]
    async fn test_websocket_consumer_ignores_non_socketio() {
        let consumer = WebSocketConsumer;
        let event = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "flask".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).await.unwrap();
        assert!(result.is_empty());
    }

    /// Verify build_builtin_bus registers consumers for all expected event kinds.
    #[tokio::test]
    async fn test_builtin_bus_has_consumers_for_all_event_kinds() {
        let (bus, _stats) = build_builtin_bus(vec![], "test".into(), PathBuf::from("."), BusOptions::default());
        // Derive the exact expected count to catch regressions when languages are added/removed.
        let lsp_count = crate::extract::EnricherRegistry::with_builtins()
            .supported_languages()
            .len();
        let expected_total =
            1 +        // ScanStatsConsumer (singleton, registered first)
            2 +        // RootDiscovered: ManifestConsumer, TreeSitterConsumer
            5 +        // RootExtracted: LanguageAccumulatorConsumer, AllEnrichmentsGate,
                       //   OpenApiConsumer, GrpcConsumer, EmbeddingIndexerConsumer
            lsp_count + // LanguageDetected: LspConsumer × N (real, Phase 3)
            3 +        // AllEnrichmentsDone: ApiLinkConsumer, TestedByConsumer,
                       //   EnrichmentFinalizer
            6 +        // FrameworkDetected: FrameworkDetectionConsumer, FastapiRouterPrefixConsumer,
                       //   SdkPathInferenceConsumer, NextjsRoutingConsumer, PubSubConsumer, WebSocketConsumer
            2;         // PassesComplete: SubsystemConsumer, LanceDBConsumer
        assert_eq!(
            bus.len(), expected_total,
            "Unexpected built-in consumer count: got {}, expected {} (lsp_count={})",
            bus.len(), expected_total, lsp_count
        );
    }

    /// Verify build_builtin_bus returns a stats handle that shares state with the bus.
    #[tokio::test]
    async fn test_builtin_bus_returns_scan_stats_handle() {
        let (mut bus, stats) = build_builtin_bus(vec![], "test".into(), PathBuf::from("."), BusOptions::default());
        // No activity yet
        assert!(!stats.read().unwrap().has_activity());

        // Emit RootDiscovered — the ScanStatsConsumer inside the bus should update stats.
        bus.emit(crate::extract::event_bus::ExtractionEvent::RootDiscovered {
            slug: "test".into(),
            path: PathBuf::from("."),
            lsp_only: false,
        }).await;
        assert!(
            stats.read().unwrap().has_activity(),
            "stats handle must reflect events fired through the bus"
        );
    }

    /// Verify EnrichmentFinalizer emits PassesComplete on empty input.
    #[tokio::test]
    async fn test_enrichment_finalizer_emits_passes_complete() {
        let consumer = EnrichmentFinalizer::new(vec![], "test".into());
        let event = ExtractionEvent::AllEnrichmentsDone {
            slug: "test".into(),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
            lsp_edges: std::sync::Arc::from([]),
            lsp_nodes: std::sync::Arc::from([]),
            updated_nodes: std::sync::Arc::from([]),
        };
        let follow_ons = consumer.on_event(&event).await.unwrap();
        assert!(
            follow_ons.iter().any(|e| matches!(e, ExtractionEvent::PassesComplete { .. })),
            "EnrichmentFinalizer must always emit PassesComplete"
        );
    }

    /// Verify ApiLinkConsumer subscribes to AllEnrichmentsDone and is a no-op.
    ///
    /// `ApiLinkConsumer` is a subscription slot — the actual api_link_pass runs
    /// inside `EnrichmentFinalizer`. This consumer emits no events.
    #[tokio::test]
    async fn test_api_link_consumer_subscription() {
        let consumer = ApiLinkConsumer;
        assert!(consumer.subscribes_to().contains(&ExtractionEventKind::AllEnrichmentsDone));
        let event = ExtractionEvent::AllEnrichmentsDone {
            slug: "test".into(),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
            lsp_edges: std::sync::Arc::from([]),
            lsp_nodes: std::sync::Arc::from([]),
            updated_nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).await.unwrap();
        assert!(result.is_empty(), "ApiLinkConsumer is a subscription slot — emits nothing");
    }

    /// Verify TestedByConsumer subscribes to AllEnrichmentsDone and is a no-op.
    ///
    /// `TestedByConsumer` is a subscription slot — the actual tested_by_pass runs
    /// inside `EnrichmentFinalizer`. This consumer emits no events.
    #[tokio::test]
    async fn test_tested_by_consumer_subscription() {
        let consumer = TestedByConsumer;
        assert!(consumer.subscribes_to().contains(&ExtractionEventKind::AllEnrichmentsDone));
        let event = ExtractionEvent::AllEnrichmentsDone {
            slug: "test".into(),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
            lsp_edges: std::sync::Arc::from([]),
            lsp_nodes: std::sync::Arc::from([]),
            updated_nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).await.unwrap();
        assert!(result.is_empty(), "TestedByConsumer is a subscription slot — emits nothing");
    }

    /// Verify FastapiRouterPrefixConsumer fires only for fastapi framework.
    #[tokio::test]
    async fn test_fastapi_router_prefix_consumer_fires_for_fastapi() {
        let consumer = FastapiRouterPrefixConsumer;
        // Matching framework
        let matching = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "fastapi".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&matching).await.unwrap();
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], ExtractionEvent::PassComplete { pass_name: "fastapi_router_prefix", .. }));

        // Non-matching
        let non_matching = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "flask".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&non_matching).await.unwrap();
        assert!(result.is_empty());
    }

    /// Adversarial: CustomExtractorConsumer fires only for its declared framework.
    #[tokio::test]
    async fn test_custom_extractor_consumer_framework_filter() {
        let consumer = CustomExtractorConsumer {
            framework: "fastapi".into(),
            config_name: "fastapi-routes".into(),
            config_bytes: vec![],
        };
        // Matching framework → fires
        let matching = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "fastapi".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&matching).await.unwrap();
        assert_eq!(result.len(), 1);

        // Non-matching → silent
        let non_matching = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "flask".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&non_matching).await.unwrap();
        assert!(result.is_empty());
    }

    /// `CustomExtractorConsumer::version()` changes when config bytes change.
    #[test]
    fn test_custom_extractor_consumer_version_from_config_bytes() {
        let cfg_v1 = b"[extractor]\nframework = 'nextjs'".to_vec();
        let cfg_v2 = b"[extractor]\nframework = 'nextjs'\nnew_field = true".to_vec();

        let c1 = CustomExtractorConsumer {
            framework: "nextjs".into(),
            config_name: "nextjs-routes".into(),
            config_bytes: cfg_v1.clone(),
        };
        let c2 = CustomExtractorConsumer {
            framework: "nextjs".into(),
            config_name: "nextjs-routes".into(),
            config_bytes: cfg_v1.clone(),
        };
        let c3 = CustomExtractorConsumer {
            framework: "nextjs".into(),
            config_name: "nextjs-routes".into(),
            config_bytes: cfg_v2.clone(),
        };

        // Same config bytes → same version.
        assert_eq!(c1.version(), c2.version(), "same config bytes must yield same version");
        // Different config bytes → different version.
        assert_ne!(c1.version(), c3.version(), "changed config bytes must yield different version");
        // Empty config bytes → stable non-panic version.
        let c_empty = CustomExtractorConsumer {
            framework: "x".into(),
            config_name: "x".into(),
            config_bytes: vec![],
        };
        let _v = c_empty.version(); // must not panic
    }

    /// `CustomExtractorConsumer::from_file` reads the file and computes version.
    #[test]
    fn test_custom_extractor_consumer_from_file() {
        let dir = TempDir::new().unwrap();
        let cfg_path = dir.path().join("my.toml");
        std::fs::write(&cfg_path, b"[extractor]\nframework = 'fastapi'").unwrap();

        let c = CustomExtractorConsumer::from_file(
            "fastapi".into(),
            "fastapi-routes".into(),
            &cfg_path,
        );
        assert_eq!(c.framework, "fastapi");
        assert!(!c.config_bytes.is_empty(), "config_bytes should be populated from file");
        // Version must be non-zero for a non-empty config file.
        // (Could be 0 in theory but statistically impossible for real TOML.)
        let _v = c.version(); // must not panic
    }

    /// Integration: bus emit sequence starting from RootDiscovered.
    /// With a real temp directory, TreeSitterConsumer produces RootExtracted.
    #[tokio::test]
    async fn test_bus_emit_root_discovered_produces_root_extracted() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/lib.rs"),
            "pub fn hello() {}\n",
        ).unwrap();
        // Need scan state dir
        std::fs::create_dir_all(tmp.path().join(".oh").join(".cache")).unwrap();

        let (mut bus, _stats) = build_builtin_bus(
            vec![("test".to_string(), tmp.path().to_path_buf())],
            "test".to_string(),
            tmp.path().to_path_buf(),
            BusOptions::default(),
        );
        let events = bus.emit(ExtractionEvent::RootDiscovered {
            slug: "test".to_string(),
            path: tmp.path().to_path_buf(),
            lsp_only: false,
        }).await;

        // Must include RootExtracted somewhere in the emitted events
        let has_root_extracted = events.iter().any(|e| matches!(e, ExtractionEvent::RootExtracted { .. }));
        assert!(has_root_extracted, "TreeSitterConsumer must produce RootExtracted from RootDiscovered");

        // Must include PassesComplete
        let has_passes_complete = events.iter().any(|e| matches!(e, ExtractionEvent::PassesComplete { .. }));
        assert!(has_passes_complete, "EnrichmentFinalizer must produce PassesComplete");
    }

    /// Verify LspConsumer fires only for its declared language.
    /// Uses a no-op enricher so the test doesn't try to start a real LSP server.
    #[tokio::test]
    async fn test_lsp_consumer_fires_for_declared_language() {
        let consumer = LspConsumer {
            language: "rust".into(),
            enricher: Arc::new(NoopEnricher { language: "rust".into() }),
            repo_root: PathBuf::from("."),
            lsp_roots: Arc::new(vec![]),
        };
        let matching = ExtractionEvent::LanguageDetected {
            slug: "test".into(),
            language: "rust".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&matching).await.unwrap();
        // No tokio runtime in sync test context: the consumer falls back to the no-op path
        // and still emits EnrichmentComplete (with empty edges).
        assert_eq!(result.len(), 1, "LspConsumer must emit EnrichmentComplete for its language");
        assert!(
            matches!(result[0], ExtractionEvent::EnrichmentComplete { .. }),
            "LspConsumer must emit EnrichmentComplete"
        );
    }

    #[tokio::test]
    async fn test_lsp_consumer_ignores_other_language() {
        let consumer = LspConsumer {
            language: "rust".into(),
            enricher: Arc::new(NoopEnricher { language: "rust".into() }),
            repo_root: PathBuf::from("."),
            lsp_roots: Arc::new(vec![]),
        };
        let other_lang = ExtractionEvent::LanguageDetected {
            slug: "test".into(),
            language: "python".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&other_lang).await.unwrap();
        assert!(result.is_empty(), "LspConsumer must ignore events for other languages");
    }

    /// Verify AllEnrichmentsGate emits AllEnrichmentsDone immediately when no supported languages.
    #[tokio::test]
    async fn test_all_enrichments_gate_no_languages() {
        let gate = AllEnrichmentsGate::new();
        // No nodes → no languages → gate fires immediately on RootExtracted.
        let event = ExtractionEvent::RootExtracted {
            slug: "test".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
            dirty_slugs: None,
        };
        let result = gate.on_event(&event).await.unwrap();
        assert_eq!(result.len(), 1, "Gate must emit AllEnrichmentsDone when expected==0");
        assert!(
            matches!(result[0], ExtractionEvent::AllEnrichmentsDone { .. }),
            "Expected AllEnrichmentsDone"
        );
    }

    /// Verify AllEnrichmentsGate waits for all enrichments before emitting.
    #[tokio::test]
    async fn test_all_enrichments_gate_waits_for_all() {
        use crate::graph::{ExtractionSource, NodeId, NodeKind};
        use std::collections::BTreeMap;

        // Create a rust node to simulate a supported language.
        let rust_node = crate::graph::Node {
            id: NodeId {
                root: "test".into(),
                file: PathBuf::from("src/lib.rs"),
                name: "foo".into(),
                kind: NodeKind::Function,
            },
            language: "rust".into(),
            line_start: 1, line_end: 1,
            signature: "foo".into(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let gate = AllEnrichmentsGate::new();

        // Send RootExtracted with 1 rust node — sets expected=1 (rust is supported).
        let root_extracted = ExtractionEvent::RootExtracted {
            slug: "test".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from(vec![rust_node].into_boxed_slice()),
            edges: std::sync::Arc::from([]),
            dirty_slugs: None,
        };
        let result = gate.on_event(&root_extracted).await.unwrap();
        // expected=1, received=0 → no AllEnrichmentsDone yet.
        assert!(result.is_empty(), "Gate must not fire before all enrichments arrive");

        // Send EnrichmentComplete for rust.
        let enrichment_done = ExtractionEvent::EnrichmentComplete {
            slug: "test".into(),
            language: "rust".into(),
            added_edges: std::sync::Arc::from([]),
            new_nodes: std::sync::Arc::from([]),
            updated_nodes: std::sync::Arc::from([]),
            server_name: None,
            error_count: 0,
            aborted: false,
        };
        let result = gate.on_event(&enrichment_done).await.unwrap();
        assert_eq!(result.len(), 1, "Gate must emit AllEnrichmentsDone after all enrichments");
        assert!(
            matches!(result[0], ExtractionEvent::AllEnrichmentsDone { .. }),
            "Expected AllEnrichmentsDone"
        );
    }

    /// Adversarial: AllEnrichmentsGate must NOT expect enrichment for clean-root languages
    /// when dirty_slugs is non-empty. A Rust node from a clean root should not count.
    #[tokio::test]
    async fn test_all_enrichments_gate_ignores_clean_root_languages() {
        use crate::graph::{ExtractionSource, NodeId, NodeKind};
        use std::collections::BTreeMap;

        // Create two rust nodes: one dirty, one clean.
        let dirty_node = crate::graph::Node {
            id: NodeId {
                root: "dirty_root".into(),
                file: PathBuf::from("src/lib.rs"),
                name: "dirty_fn".into(),
                kind: NodeKind::Function,
            },
            language: "rust".into(),
            line_start: 1, line_end: 1,
            signature: "dirty_fn".into(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let clean_node = crate::graph::Node {
            id: NodeId {
                root: "clean_root".into(),
                file: PathBuf::from("src/lib.rs"),
                name: "clean_fn".into(),
                kind: NodeKind::Function,
            },
            language: "python".into(), // different language so we can detect if it's counted
            line_start: 1, line_end: 1,
            signature: "clean_fn".into(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let gate = AllEnrichmentsGate::new();

        // Only dirty_root is dirty. clean_root's python should not count.
        let root_extracted = ExtractionEvent::RootExtracted {
            slug: "test".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from(vec![dirty_node, clean_node].into_boxed_slice()),
            edges: std::sync::Arc::from([]),
            dirty_slugs: Some(std::collections::HashSet::from(["dirty_root".to_string()])),
        };
        let result = gate.on_event(&root_extracted).await.unwrap();
        // expected=1 (only rust from dirty_root), not 2 (rust + python).
        // Gate should NOT fire yet (still waiting for rust EnrichmentComplete).
        assert!(result.is_empty(), "Gate must not fire before dirty-root enrichments arrive");

        // Send EnrichmentComplete for rust only.
        let enrichment_done = ExtractionEvent::EnrichmentComplete {
            slug: "test".into(),
            language: "rust".into(),
            added_edges: std::sync::Arc::from([]),
            new_nodes: std::sync::Arc::from([]),
            updated_nodes: std::sync::Arc::from([]),
            server_name: None,
            error_count: 0,
            aborted: false,
        };
        let result = gate.on_event(&enrichment_done).await.unwrap();
        // Should fire now: expected=1 (rust), received=1 (rust).
        // If python from clean_root were counted, we'd still be waiting.
        assert_eq!(result.len(), 1, "Gate must fire after dirty-root enrichment completes (clean-root python must not block)");
        assert!(
            matches!(result[0], ExtractionEvent::AllEnrichmentsDone { .. }),
            "Expected AllEnrichmentsDone"
        );
    }

    /// Adversarial: empty dirty_slugs treats all roots as dirty (backward compatibility).
    #[tokio::test]
    async fn test_all_enrichments_gate_empty_dirty_slugs_all_dirty() {
        use crate::graph::{ExtractionSource, NodeId, NodeKind};
        use std::collections::BTreeMap;

        let node_a = crate::graph::Node {
            id: NodeId {
                root: "root_a".into(),
                file: PathBuf::from("src/lib.rs"),
                name: "fn_a".into(),
                kind: NodeKind::Function,
            },
            language: "rust".into(),
            line_start: 1, line_end: 1,
            signature: "fn_a".into(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };
        let node_b = crate::graph::Node {
            id: NodeId {
                root: "root_b".into(),
                file: PathBuf::from("app.py"),
                name: "fn_b".into(),
                kind: NodeKind::Function,
            },
            language: "python".into(),
            line_start: 1, line_end: 1,
            signature: "fn_b".into(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let gate = AllEnrichmentsGate::new();

        // Empty dirty_slugs = all dirty. Both languages should be expected.
        let root_extracted = ExtractionEvent::RootExtracted {
            slug: "test".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from(vec![node_a, node_b].into_boxed_slice()),
            edges: std::sync::Arc::from([]),
            dirty_slugs: None, // None = all dirty
        };
        let result = gate.on_event(&root_extracted).await.unwrap();
        assert!(result.is_empty(), "Gate must wait for both languages when all dirty");

        // Complete rust -- still waiting for python.
        let rust_done = ExtractionEvent::EnrichmentComplete {
            slug: "test".into(),
            language: "rust".into(),
            added_edges: std::sync::Arc::from([]),
            new_nodes: std::sync::Arc::from([]),
            updated_nodes: std::sync::Arc::from([]),
            server_name: None,
            error_count: 0,
            aborted: false,
        };
        let result = gate.on_event(&rust_done).await.unwrap();
        assert!(result.is_empty(), "Gate must wait for python too when dirty_slugs is empty");

        // Complete python -- now both done.
        let python_done = ExtractionEvent::EnrichmentComplete {
            slug: "test".into(),
            language: "python".into(),
            added_edges: std::sync::Arc::from([]),
            new_nodes: std::sync::Arc::from([]),
            updated_nodes: std::sync::Arc::from([]),
            server_name: None,
            error_count: 0,
            aborted: false,
        };
        let result = gate.on_event(&python_done).await.unwrap();
        assert_eq!(result.len(), 1, "Gate must fire after all dirty-root enrichments complete");
    }

    /// Verify SubsystemConsumer subscribes to CommunityDetectionComplete and emits
    /// SubsystemNodesComplete (even when no subsystems are detected).
    #[tokio::test]
    async fn test_subsystem_consumer_subscription() {
        let consumer = SubsystemConsumer;
        assert!(
            consumer.subscribes_to().contains(&ExtractionEventKind::CommunityDetectionComplete),
            "SubsystemConsumer must subscribe to CommunityDetectionComplete"
        );
        let event = ExtractionEvent::CommunityDetectionComplete {
            slug: "test".into(),
            subsystems: std::sync::Arc::from([]),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).await.unwrap();
        assert_eq!(result.len(), 1, "SubsystemConsumer must emit exactly one SubsystemNodesComplete");
        assert!(
            matches!(result[0], ExtractionEvent::SubsystemNodesComplete { .. }),
            "SubsystemConsumer must emit SubsystemNodesComplete"
        );
    }

    /// Verify LanceDBConsumer (stub) subscribes to PassesComplete and emits nothing.
    #[tokio::test]
    async fn test_lancedb_consumer_subscription() {
        let consumer = LanceDBConsumer::stub();
        assert!(consumer.subscribes_to().contains(&ExtractionEventKind::PassesComplete));
        let event = ExtractionEvent::PassesComplete {
            slug: "test".into(),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
            detected_frameworks: HashSet::new(),
        };
        let result = consumer.on_event(&event).await.unwrap();
        assert!(result.is_empty(), "LanceDBConsumer stub emits nothing");
    }

    /// Verify EmbeddingIndexerConsumer (stub) subscribes to RootExtracted and emits nothing.
    #[tokio::test]
    async fn test_embedding_indexer_consumer_subscription() {
        let consumer = EmbeddingIndexerConsumer::stub();
        assert!(consumer.subscribes_to().contains(&ExtractionEventKind::RootExtracted));
        let event = ExtractionEvent::RootExtracted {
            slug: "test".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
            dirty_slugs: None,
        };
        let result = consumer.on_event(&event).await.unwrap();
        assert!(result.is_empty(), "EmbeddingIndexerConsumer stub emits nothing");
    }

    // ── emit_enrichment_pipeline tests ─────────────────────────────────────

    /// Verify emit_enrichment_pipeline returns Ok(PassesComplete data) on empty input.
    #[tokio::test]
    async fn test_emit_enrichment_pipeline_empty_input() {
        let (nodes, edges, frameworks) = super::emit_enrichment_pipeline(
            vec![],
            vec![],
            vec![],
            "test".to_string(),
            PathBuf::from("."),
            super::BusOptions::default(),
            None,
        ).await.expect("emit_enrichment_pipeline must not fail on empty input");
        assert!(nodes.is_empty(), "empty input → empty nodes");
        assert!(edges.is_empty(), "empty input → empty edges");
        assert!(frameworks.is_empty(), "empty input → no frameworks");
    }

    /// Verify emit_enrichment_pipeline routes nodes through EnrichmentFinalizer.
    #[tokio::test]
    async fn test_emit_enrichment_pipeline_preserves_input_nodes() {
        use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};
        use std::collections::BTreeMap;

        let node = Node {
            id: NodeId {
                root: "test".into(),
                file: PathBuf::from("src/lib.rs"),
                name: "my_fn".into(),
                kind: NodeKind::Function,
            },
            language: "rust".into(),
            line_start: 1,
            line_end: 1,
            signature: "fn my_fn()".into(),
            body: "fn my_fn() {}".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let input_nodes = vec![node];
        let (out_nodes, _out_edges, _frameworks) = super::emit_enrichment_pipeline(
            input_nodes,
            vec![],
            vec![],
            "test".to_string(),
            PathBuf::from("."),
            super::BusOptions::default(),
            None,
        ).await.expect("emit_enrichment_pipeline must not fail with valid input");

        assert!(
            out_nodes.iter().any(|n| n.id.name == "my_fn"),
            "emit_enrichment_pipeline must preserve input nodes through enrichment passes"
        );
    }

    // -----------------------------------------------------------------------
    // Adversarial tests: framework-gated pass ordering (#553)
    // -----------------------------------------------------------------------

    /// Adversarial: `SdkPathInferenceConsumer` fires only for fastapi, silent for others.
    #[tokio::test]
    async fn test_sdk_path_inference_consumer_fires_only_for_fastapi() {
        let consumer = SdkPathInferenceConsumer;
        assert!(consumer.subscribes_to().contains(&ExtractionEventKind::FrameworkDetected));

        // Must fire for fastapi
        let fastapi_event = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "fastapi".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&fastapi_event).await.unwrap();
        assert_eq!(result.len(), 1, "SdkPathInferenceConsumer must fire for fastapi");
        assert!(matches!(result[0], ExtractionEvent::PassComplete { pass_name: "sdk_path_inference", .. }));

        // Must be silent for unrelated frameworks
        for framework in &["flask", "django", "nextjs-app-router", "react", "socketio"] {
            let other_event = ExtractionEvent::FrameworkDetected {
                slug: "test".into(),
                framework: framework.to_string(),
                nodes: std::sync::Arc::from([]),
            };
            let result = consumer.on_event(&other_event).await.unwrap();
            assert!(
                result.is_empty(),
                "SdkPathInferenceConsumer must be silent for framework '{}', got {:?}",
                framework, result,
            );
        }
    }

    /// Adversarial: plain React repo (TS but no `next/` imports, no `pages/` dir)
    /// does NOT invoke nextjs_routing_pass and produces no ApiEndpoint nodes.
    ///
    /// Verifies the double gate: import-based AND filesystem-based detection are
    /// both absent → `nextjs_routing_pass` is not invoked → no ApiEndpoint nodes.
    #[tokio::test]
    async fn test_plain_ts_repo_without_nextjs_does_not_produce_api_endpoints() {
        use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};
        use std::collections::BTreeMap;

        // Plain TypeScript file with a React import (but NOT next/)
        let import_node = Node {
            id: NodeId {
                root: "client".into(),
                file: PathBuf::from("src/App.tsx"),
                name: "import react".into(),
                kind: NodeKind::Import,
            },
            language: "typescript".into(),
            line_start: 1,
            line_end: 1,
            signature: "import React from 'react'".into(),
            body: "import React from 'react'".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let (out_nodes, _edges, _frameworks) = super::emit_enrichment_pipeline(
            vec![import_node],
            vec![],
            vec![("client".into(), PathBuf::from("."))],
            "client".to_string(),
            PathBuf::from("."),
            super::BusOptions::default(),
            None,
        ).await.unwrap();

        // Must produce no ApiEndpoint nodes — nextjs_routing_pass must NOT fire
        let api_endpoints: Vec<_> = out_nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert!(
            api_endpoints.is_empty(),
            "Plain TS/React repo without `next/` imports must produce no ApiEndpoint nodes. \
            Got: {:?}",
            api_endpoints.iter().map(|n| &n.id.name).collect::<Vec<_>>(),
        );
    }

    /// Adversarial: Next.js project with pages/ dir but no next/ imports still gets routing.
    ///
    /// Verifies the filesystem-based fallback: a repo with `pages/api/` directory
    /// but no `next/` imports (e.g., plain TypeScript handler functions) should
    /// still have `nextjs_routing_pass` invoked.
    #[tokio::test]
    async fn test_nextjs_with_pages_dir_but_no_imports_gets_routing() {
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let pages_api = dir.path().join("pages/api");
        std::fs::create_dir_all(&pages_api).unwrap();
        std::fs::write(
            pages_api.join("health.ts"),
            "export default function handler(req: any, res: any) { res.json({ ok: true }); }\n",
        ).unwrap();

        // TypeScript import node but no `next/` import — pure handler, no next imports
        use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};
        use std::collections::BTreeMap;
        let fn_node = Node {
            id: NodeId {
                root: "client".into(),
                file: PathBuf::from("pages/api/health.ts"),
                name: "handler".into(),
                kind: NodeKind::Function,
            },
            language: "typescript".into(),
            line_start: 1,
            line_end: 1,
            signature: "function handler(req, res)".into(),
            body: "export default function handler(req, res) { res.json({ ok: true }); }".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        let root_path = dir.path().to_path_buf();
        let (out_nodes, _edges, _frameworks) = super::emit_enrichment_pipeline(
            vec![fn_node],
            vec![],
            vec![("client".into(), root_path)],
            "client".to_string(),
            PathBuf::from("."),
            super::BusOptions::default(),
            None,
        ).await.unwrap();

        // Must find an ApiEndpoint node — filesystem-based gate must fire
        let api_endpoints: Vec<_> = out_nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert!(
            !api_endpoints.is_empty(),
            "Next.js repo with pages/ dir but no next/ imports must produce ApiEndpoint \
            nodes via filesystem-based detection. Got nodes: {:?}",
            out_nodes.iter().map(|n| (&n.id.kind, &n.id.name)).collect::<Vec<_>>(),
        );
    }

    /// Adversarial: non-FastAPI repo does NOT invoke fastapi_router_prefix or sdk_path_inference.
    ///
    /// Verifies that framework detection gates both FastAPI passes correctly.
    /// Runs `emit_enrichment_pipeline` with only a React import node — neither
    /// `fastapi_router_prefix_pass` nor `sdk_path_inference_pass` should be invoked.
    /// (Absence of errors + no state mutation from those passes = correct.)
    #[tokio::test]
    async fn test_non_fastapi_repo_does_not_invoke_fastapi_passes() {
        use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};
        use std::collections::BTreeMap;

        // Repo with only React TypeScript — no FastAPI
        let import_node = Node {
            id: NodeId {
                root: "app".into(),
                file: PathBuf::from("src/App.tsx"),
                name: "import react".into(),
                kind: NodeKind::Import,
            },
            language: "typescript".into(),
            line_start: 1,
            line_end: 1,
            signature: "import React from 'react'".into(),
            body: "import React from 'react'".into(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        };

        // If fastapi_router_prefix_pass or sdk_path_inference_pass were invoked,
        // they would scan all nodes looking for Python ApiEndpoint nodes.
        // They produce no output on this input, but verifying `(_frameworks)` does
        // not contain "fastapi" is the structural proof that the gate worked.
        let (_out_nodes, _edges, frameworks) = super::emit_enrichment_pipeline(
            vec![import_node],
            vec![],
            vec![("app".into(), PathBuf::from("."))],
            "app".to_string(),
            PathBuf::from("."),
            super::BusOptions::default(),
            None,
        ).await.unwrap();

        assert!(
            !frameworks.contains("fastapi"),
            "Non-FastAPI repo must not have 'fastapi' in detected_frameworks"
        );
    }
}
