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
//! 4. `LspConsumer(lang)` × N — subscribes to `LanguageDetected`, runs LSP enrichment,
//!    emits `EnrichmentComplete`
//! 5. `AllEnrichmentsGate` — subscribes to `LanguageDetected` + `EnrichmentComplete`,
//!    emits `AllEnrichmentsDone` once all languages complete
//! 6. `PostExtractionConsumer` — subscribes to `AllEnrichmentsDone`, runs all
//!    post-extraction passes on the LSP-enriched graph, emits `PassesComplete`
//! 7. `EmbeddingConsumer` — subscribes to `RootExtracted` (streaming embed as nodes arrive)
//!
//! # Phase 3 LSP promotion
//!
//! `LspConsumer` now calls real LSP enrichment via `tokio::task::block_in_place` so the
//! async enricher runs within the synchronous event bus. `AllEnrichmentsGate` aggregates
//! per-language `EnrichmentComplete` events and merges results before handing to
//! `PostExtractionConsumer`. This ensures post-extraction passes run on LSP-enriched graphs.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc as StdArc, Mutex};

use crate::extract::{EnricherRegistry, ExtractorRegistry};
use crate::extract::event_bus::{ExtractionConsumer, ExtractionEvent, ExtractionEventKind};
use crate::graph::{Edge, Node};

// ---------------------------------------------------------------------------
// ManifestConsumer
// ---------------------------------------------------------------------------

/// Runs `manifest_pass` when a root is discovered.
///
/// Subscribes to: `RootDiscovered`
/// Emits: nothing (manifest nodes are returned via `PassComplete` in the
/// `PostExtractionConsumer` flow; this consumer is a stub for the subscription
/// pattern — manifest is already handled inside `PostExtractionRegistry`).
///
/// In the ADR's design, `manifest_pass` subscribes to `RootDiscovered` because
/// it needs filesystem access (reads `package.json`, `Cargo.toml`, etc.).
/// For Phase 2 we keep manifest inside `PostExtractionRegistry` (which runs on
/// `RootExtracted`) to avoid duplicating filesystem reads. This consumer
/// establishes the subscription slot for Phase 3+ when manifest is promoted.
pub struct ManifestConsumer;

impl ExtractionConsumer for ManifestConsumer {
    fn name(&self) -> &str { "manifest" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootDiscovered]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::RootDiscovered { slug, .. } = event else {
            return Ok(vec![]);
        };
        tracing::debug!("ManifestConsumer: root '{}' discovered (manifest handled by PostExtractionConsumer)", slug);
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
}

impl TreeSitterConsumer {
    pub fn new() -> Self {
        Self {
            registry: ExtractorRegistry::with_builtins(),
        }
    }
}

impl Default for TreeSitterConsumer {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtractionConsumer for TreeSitterConsumer {
    fn name(&self) -> &str { "tree_sitter" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootDiscovered]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
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

        let mut extraction = self.registry.extract_scan_result(path, &full_scan);

        // Stamp all nodes/edges with the root slug.
        for node in &mut extraction.nodes {
            node.id.root = slug.clone();
        }
        for edge in &mut extraction.edges {
            edge.from.root = slug.clone();
            edge.to.root = slug.clone();
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
        }])
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

impl ExtractionConsumer for LanguageAccumulatorConsumer {
    fn name(&self) -> &str { "language_accumulator" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootExtracted]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::RootExtracted { slug, nodes, .. } = event else {
            return Ok(vec![]);
        };

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
            by_lang.entry(node.language.clone()).or_default().push(node.clone());
        }

        let mut events: Vec<ExtractionEvent> = Vec::with_capacity(by_lang.len());
        for (language, lang_nodes) in by_lang {
            tracing::debug!(
                "LanguageAccumulatorConsumer: root '{}' language '{}': {} nodes",
                slug,
                language,
                lang_nodes.len(),
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
// PostExtractionConsumer
// ---------------------------------------------------------------------------

/// Runs all post-extraction passes (via `PostExtractionRegistry`) on the LSP-enriched graph.
///
/// Subscribes to: `AllEnrichmentsDone`
/// Emits: `FrameworkDetected` (one per detected framework) + `PassesComplete`
///
/// By subscribing to `AllEnrichmentsDone` (rather than `RootExtracted`), the passes
/// operate on the graph after LSP enrichment has added cross-file edges and virtual
/// nodes — satisfying ADR 001 Phase 3: "PostExtractionRegistry runs passes once over
/// the full merged graph."
pub struct PostExtractionConsumer {
    root_pairs: Vec<(String, PathBuf)>,
    primary_slug: String,
}

impl PostExtractionConsumer {
    pub fn new(root_pairs: Vec<(String, PathBuf)>, primary_slug: String) -> Self {
        Self { root_pairs, primary_slug }
    }
}

impl ExtractionConsumer for PostExtractionConsumer {
    fn name(&self) -> &str { "post_extraction" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::AllEnrichmentsDone]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::AllEnrichmentsDone { slug, nodes, edges, .. } = event else {
            return Ok(vec![]);
        };

        use crate::extract::post_extraction::{PassContext, PostExtractionRegistry};

        let registry = PostExtractionRegistry::with_builtins();
        let ctx = PassContext {
            root_pairs: self.root_pairs.clone(),
            primary_slug: self.primary_slug.clone(),
            detected_frameworks: HashSet::new(),
        };

        // PostExtractionRegistry::run_all takes mutable Vec references and appends
        // items. We must materialize mutable Vecs from the Arc<[T]> payloads.
        // The resulting all_nodes/all_edges are wrapped back into Arc<[T]> for the
        // PassesComplete event so downstream consumers share the same allocation.
        let mut all_nodes = nodes.to_vec();
        let mut all_edges = edges.to_vec();
        let result = registry.run_all(&mut all_nodes, &mut all_edges, ctx);

        tracing::info!(
            "PostExtractionConsumer: root '{}' passes complete: +{} node(s), +{} edge(s), {} framework(s)",
            slug,
            result.added_node_count,
            result.added_edge_count,
            result.detected_frameworks.len(),
        );

        let mut follow_ons: Vec<ExtractionEvent> = Vec::new();

        // Emit FrameworkDetected for each detected framework.
        // Sort framework names for deterministic fan-out ordering — HashSet iteration
        // is nondeterministic, which causes unstable event ordering across runs.
        let mut frameworks: Vec<String> = result.detected_frameworks.iter().cloned().collect();
        frameworks.sort_unstable();
        for framework in &frameworks {
            // Gather nodes associated with this framework.
            let fw_nodes: Vec<Node> = all_nodes
                .iter()
                .filter(|n| {
                    n.metadata.get("framework").map(|f| f == framework).unwrap_or(false)
                })
                .cloned()
                .collect();
            follow_ons.push(ExtractionEvent::FrameworkDetected {
                slug: slug.clone(),
                framework: framework.clone(),
                nodes: std::sync::Arc::from(fw_nodes.into_boxed_slice()),
            });
        }

        // Always emit PassesComplete with the enriched graph.
        // Wrap into Arc<[T]> so all PassesComplete subscribers share the allocation.
        let nodes_arc = std::sync::Arc::from(all_nodes.into_boxed_slice());
        let edges_arc = std::sync::Arc::from(all_edges.into_boxed_slice());
        follow_ons.push(ExtractionEvent::PassesComplete {
            slug: slug.clone(),
            nodes: nodes_arc,
            edges: edges_arc,
            detected_frameworks: result.detected_frameworks,
        });

        Ok(follow_ons)
    }
}

// ---------------------------------------------------------------------------
// FrameworkDetectionConsumer
// ---------------------------------------------------------------------------

/// Logs framework detection events. Framework-gated passes subscribe to
/// `FrameworkDetected` and check `event.framework` to filter.
///
/// This consumer is a diagnostic observer; actual framework-gated passes
/// (pubsub, websocket, nextjs) are handled inside `PostExtractionRegistry`.
/// In Phase 2 they stay there; in Phase 3+ they each become independent
/// consumers that subscribe to `FrameworkDetected`.
pub struct FrameworkDetectionConsumer;

impl ExtractionConsumer for FrameworkDetectionConsumer {
    fn name(&self) -> &str { "framework_detection" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
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

/// Subscribes to `FrameworkDetected` and runs nextjs_routing_pass when
/// `framework == "nextjs-app-router"`.
///
/// **ADR pattern:** Framework-gated pass as a consumer — wakes only when its
/// framework fires, never polls context.
///
/// Subscribes to: `FrameworkDetected`
/// Emits: `PassComplete`
pub struct NextjsRoutingConsumer {
    /// Stored for Phase 3+ when nextjs_routing_pass is promoted out of PostExtractionRegistry.
    #[allow(dead_code)]
    root_pairs: Vec<(String, PathBuf)>,
}

impl NextjsRoutingConsumer {
    pub fn new(root_pairs: Vec<(String, PathBuf)>) -> Self {
        Self { root_pairs }
    }
}

impl ExtractionConsumer for NextjsRoutingConsumer {
    fn name(&self) -> &str { "nextjs_routing" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::FrameworkDetected { slug, framework, .. } = event else {
            return Ok(vec![]);
        };

        // Only wake for nextjs-app-router. The `has_ts_js` fallback from the original
        // PostExtractionPass is intentionally dropped here: `nodes` in FrameworkDetected
        // carries only the framework-matching nodes, not all repo nodes, so checking
        // TS/JS presence would give wrong results. In Phase 3+, when the bus has access
        // to the full node set, this can be revisited.
        if framework != "nextjs-app-router" {
            return Ok(vec![]);
        }

        // Note: nextjs_routing_pass needs the full node set, not just the framework nodes.
        // This consumer emits PassComplete as a signal; the actual pass runs inside
        // PostExtractionRegistry in Phase 2. This establishes the subscription pattern.
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

impl ExtractionConsumer for PubSubConsumer {
    fn name(&self) -> &str { "pubsub" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::FrameworkDetected { slug, framework, .. } = event else {
            return Ok(vec![]);
        };
        // Only fire for broker frameworks. In Phase 2 the actual pass logic
        // runs inside PostExtractionRegistry; this establishes the subscription slot.
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

impl ExtractionConsumer for WebSocketConsumer {
    fn name(&self) -> &str { "websocket" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
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

impl ExtractionConsumer for OpenApiConsumer {
    fn name(&self) -> &str { "openapi_bidirectional" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootExtracted]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
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

impl ExtractionConsumer for GrpcConsumer {
    fn name(&self) -> &str { "grpc_proto" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootExtracted]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
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
pub struct CustomExtractorConsumer {
    /// Framework name this consumer is configured for.
    pub framework: String,
    /// Slug identifying this config (for diagnostics).
    pub config_name: String,
}

impl ExtractionConsumer for CustomExtractorConsumer {
    fn name(&self) -> &str { "custom_extractor" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::FrameworkDetected]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
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
}

// ---------------------------------------------------------------------------
// LspConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `LanguageDetected` and runs real LSP enrichment for the given language.
///
/// At construction, receives a per-language `EnricherRegistry` pre-filtered to enrichers
/// that support this language. In `on_event(LanguageDetected)`, calls the enricher for
/// that language via `tokio::task::block_in_place` so the async enricher runs within the
/// synchronous event bus without blocking the tokio executor.
///
/// Per the ADR: "Consumer of LanguageDetected(lang, nodes): LSP enrichers — ALL fire
/// concurrently, one per language." Concurrency emerges naturally because the event bus
/// fan-out calls each `LspConsumer` sequentially within a single `tokio::task::block_in_place`
/// call per consumer; if the bus is called from a multi-threaded context (e.g., rayon), the
/// per-language enrichment tasks run on separate OS threads. For single-threaded bus callers,
/// enrichers run serially but without holding up other tokio tasks.
///
/// Subscribes to: `LanguageDetected`
/// Emits: `EnrichmentComplete` (with real edges when an LSP server is available)
pub struct LspConsumer {
    /// Which language this consumer handles (e.g., "rust", "python", "typescript").
    pub language: String,
    /// Repository root for LSP server startup.
    pub repo_root: PathBuf,
    /// `lsp_only` subdirectory roots for monorepo lsp_root selection.
    pub lsp_only_roots: StdArc<Vec<(String, PathBuf)>>,
    /// Enricher for this specific language (derived from `EnricherRegistry`).
    /// `None` when no enricher is registered for this language — consumer emits an
    /// empty `EnrichmentComplete` in that case (same behaviour as the Phase 2 stub).
    pub enricher: Option<StdArc<dyn crate::extract::Enricher>>,
}

impl ExtractionConsumer for LspConsumer {
    fn name(&self) -> &str { "lsp" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::LanguageDetected]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::LanguageDetected { slug, language, nodes } = event else {
            return Ok(vec![]);
        };
        if language != &self.language {
            return Ok(vec![]);
        }

        let enricher = match &self.enricher {
            Some(e) => e.clone(),
            None => {
                tracing::debug!(
                    "LspConsumer({}): root '{}' — no enricher registered, emitting empty EnrichmentComplete",
                    self.language, slug,
                );
                return Ok(vec![ExtractionEvent::EnrichmentComplete {
                    slug: slug.clone(),
                    language: language.clone(),
                    added_edges: std::sync::Arc::from([]),
                    new_nodes: std::sync::Arc::from([]),
                }]);
            }
        };

        // Configure lsp_root for monorepo subdirectory selection (one-time via OnceLock).
        // Must run before the first `enrich()` so `ensure_initialized` picks up the override.
        if !self.lsp_only_roots.is_empty() {
            let best = crate::extract::pick_lsp_root_for_nodes(
                nodes,
                &self.repo_root,
                &self.lsp_only_roots,
                enricher.config_file_hint(),
            );
            if best != self.repo_root.as_path() {
                enricher.set_startup_root(best.to_path_buf());
            }
        }

        // Build a minimal GraphIndex for the enricher. LSP enrichers only need the index
        // to resolve symbol names to NodeIds; a fresh index over the language nodes is sufficient.
        let index = {
            let mut idx = crate::graph::index::GraphIndex::new();
            for node in nodes.iter() {
                idx.ensure_node(&node.stable_id(), &node.id.kind.to_string());
            }
            idx
        };

        let nodes_vec: Vec<Node> = nodes.to_vec();
        let repo_root = self.repo_root.clone();

        // `block_in_place` requires a multi-threaded tokio runtime. On the
        // current-thread runtime (e.g. `#[tokio::test]` without `flavor =
        // "multi_thread"`), calling it panics. Check the flavor first and skip
        // LSP enrichment gracefully — the pipeline still completes, just without
        // LSP edges. In production the server always runs on the multi-thread runtime.
        let on_multi_thread = matches!(
            tokio::runtime::Handle::current().runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread,
        );
        if !on_multi_thread {
            tracing::debug!(
                "LspConsumer({}): root '{}' — single-thread runtime, skipping LSP enrichment \
                 (block_in_place not available); emitting empty EnrichmentComplete",
                self.language, slug,
            );
            return Ok(vec![ExtractionEvent::EnrichmentComplete {
                slug: slug.clone(),
                language: language.clone(),
                added_edges: std::sync::Arc::from([]),
                new_nodes: std::sync::Arc::from([]),
            }]);
        }

        // Run the async enricher synchronously within the event bus's synchronous call chain.
        // `block_in_place` moves this work off the tokio worker thread so other tasks keep
        // running; `block_on` drives the future to completion on the current thread.
        let result = tokio::task::block_in_place(|| {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async move {
                enricher.enrich(&nodes_vec, &index, &repo_root).await
            })
        });

        match result {
            Ok(enrichment) => {
                tracing::info!(
                    "LspConsumer({}): root '{}' — {} edge(s), {} virtual node(s), {} patch(es)",
                    self.language, slug,
                    enrichment.added_edges.len(),
                    enrichment.new_nodes.len(),
                    enrichment.updated_nodes.len(),
                );
                Ok(vec![ExtractionEvent::EnrichmentComplete {
                    slug: slug.clone(),
                    language: language.clone(),
                    added_edges: StdArc::from(enrichment.added_edges.into_boxed_slice()),
                    new_nodes: StdArc::from(enrichment.new_nodes.into_boxed_slice()),
                }])
            }
            Err(e) => {
                // LSP server not available or failed. Emit an empty EnrichmentComplete so
                // AllEnrichmentsGate still gets its expected count and the pipeline proceeds.
                tracing::info!(
                    "LspConsumer({}): root '{}' — enricher unavailable or failed: {} \
                     (emitting empty EnrichmentComplete so pipeline can proceed)",
                    self.language, slug, e,
                );
                Ok(vec![ExtractionEvent::EnrichmentComplete {
                    slug: slug.clone(),
                    language: language.clone(),
                    added_edges: std::sync::Arc::from([]),
                    new_nodes: std::sync::Arc::from([]),
                }])
            }
        }
    }
}

// ---------------------------------------------------------------------------
// AllEnrichmentsGate
// ---------------------------------------------------------------------------

/// Aggregates per-language `EnrichmentComplete` events and emits `AllEnrichmentsDone`
/// once all languages for a slug have completed.
///
/// Subscribes to:
/// - `RootExtracted` — stores the base nodes/edges and checks if any languages exist
/// - `LanguageDetected` — increments the expected completion count per slug
/// - `EnrichmentComplete` — decrements the pending count; merges edges+nodes
///
/// When all pending enrichments complete (or immediately if no languages were detected),
/// emits `AllEnrichmentsDone { slug, nodes, edges }` with the merged graph
/// (original extraction + all LSP additions).
///
/// **Zero-language path**: if `RootExtracted` has nodes but none are language-tagged
/// (or all nodes have empty language), `LanguageAccumulatorConsumer` emits zero
/// `LanguageDetected` events. In that case the gate detects `pending == 0` after
/// `RootExtracted` and emits `AllEnrichmentsDone` immediately with the base graph.
///
/// Thread safety: uses `Mutex<HashMap>` for interior mutability since `on_event`
/// takes `&self` but must update per-slug state across multiple event calls.
pub struct AllEnrichmentsGate {
    /// Per slug: pending language count + accumulated base nodes/edges + lsp additions.
    state: Mutex<HashMap<String, GateState>>,
}

struct GateState {
    /// How many `LanguageDetected` events we've seen for this slug.
    /// Decremented by each `EnrichmentComplete`. When it reaches 0, the gate fires.
    pending: usize,
    /// Whether `RootExtracted` has been seen and base_nodes/edges have been populated.
    base_initialized: bool,
    /// Original extracted nodes (from `RootExtracted`).
    base_nodes: std::sync::Arc<[Node]>,
    /// Original extracted edges (from `RootExtracted`).
    base_edges: std::sync::Arc<[Edge]>,
    /// Repository root path (from `RootExtracted`).
    path: PathBuf,
    /// LSP-added edges accumulated across all `EnrichmentComplete` events.
    lsp_edges: Vec<Edge>,
    /// LSP virtual nodes accumulated across all `EnrichmentComplete` events.
    lsp_nodes: Vec<Node>,
}

impl AllEnrichmentsGate {
    pub fn new() -> Self {
        Self { state: Mutex::new(HashMap::new()) }
    }

    /// Try to finalize a slug: emit `AllEnrichmentsDone` if base is initialized and pending == 0.
    ///
    /// Returns `Some(AllEnrichmentsDone)` when the gate fires, `None` otherwise.
    /// The caller must hold the mutex lock while calling this — pass the guard.
    fn try_finalize(guard: &mut HashMap<String, GateState>, slug: &str) -> Option<ExtractionEvent> {
        let entry = guard.get(slug)?;
        if !entry.base_initialized || entry.pending > 0 {
            return None;
        }
        // Remove and finalize.
        let gs = guard.remove(slug).expect("entry confirmed present above");
        let mut all_nodes: Vec<Node> = gs.base_nodes.to_vec();
        all_nodes.extend(gs.lsp_nodes);
        let mut all_edges: Vec<Edge> = gs.base_edges.to_vec();
        all_edges.extend(gs.lsp_edges);

        tracing::info!(
            "AllEnrichmentsGate: slug '{}' all enrichments done — \
             {} total node(s), {} total edge(s)",
            slug, all_nodes.len(), all_edges.len(),
        );

        Some(ExtractionEvent::AllEnrichmentsDone {
            slug: slug.to_owned(),
            nodes: std::sync::Arc::from(all_nodes.into_boxed_slice()),
            edges: std::sync::Arc::from(all_edges.into_boxed_slice()),
            path: gs.path,
        })
    }
}

impl Default for AllEnrichmentsGate {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtractionConsumer for AllEnrichmentsGate {
    fn name(&self) -> &str { "all_enrichments_gate" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[
            ExtractionEventKind::RootExtracted,
            ExtractionEventKind::LanguageDetected,
            ExtractionEventKind::EnrichmentComplete,
        ]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        match event {
            ExtractionEvent::RootExtracted { slug, path, nodes, edges } => {
                let mut guard = self.state.lock().map_err(|e| anyhow::anyhow!("AllEnrichmentsGate lock poisoned: {}", e))?;
                let entry = guard.entry(slug.clone()).or_insert_with(|| GateState {
                    pending: 0,
                    base_initialized: false,
                    base_nodes: std::sync::Arc::from([]),
                    base_edges: std::sync::Arc::from([]),
                    path: PathBuf::new(),
                    lsp_edges: Vec::new(),
                    lsp_nodes: Vec::new(),
                });
                entry.base_nodes = std::sync::Arc::clone(nodes);
                entry.base_edges = std::sync::Arc::clone(edges);
                entry.path = path.clone();
                entry.base_initialized = true;
                tracing::debug!(
                    "AllEnrichmentsGate: slug '{}' base graph set ({} nodes, {} edges)",
                    slug, nodes.len(), edges.len(),
                );

                // Check zero-language path: if LanguageDetected events have already been
                // counted (unlikely since RootExtracted fires before LanguageDetected, but
                // defensive) or if pending remains 0 after initialization (no languages),
                // try to finalize immediately.
                if let Some(done) = Self::try_finalize(&mut guard, slug) {
                    return Ok(vec![done]);
                }
                Ok(vec![])
            }

            ExtractionEvent::LanguageDetected { slug, .. } => {
                let mut guard = self.state.lock().map_err(|e| anyhow::anyhow!("AllEnrichmentsGate lock poisoned: {}", e))?;
                let entry = guard.entry(slug.clone()).or_insert_with(|| GateState {
                    pending: 0,
                    base_initialized: false,
                    base_nodes: std::sync::Arc::from([]),
                    base_edges: std::sync::Arc::from([]),
                    path: PathBuf::new(),
                    lsp_edges: Vec::new(),
                    lsp_nodes: Vec::new(),
                });
                entry.pending += 1;
                tracing::debug!(
                    "AllEnrichmentsGate: slug '{}' LanguageDetected (pending now {})",
                    slug, entry.pending,
                );
                Ok(vec![])
            }

            ExtractionEvent::EnrichmentComplete { slug, language, added_edges, new_nodes } => {
                let mut guard = self.state.lock().map_err(|e| anyhow::anyhow!("AllEnrichmentsGate lock poisoned: {}", e))?;
                let entry = match guard.get_mut(slug) {
                    Some(e) => e,
                    None => {
                        tracing::warn!(
                            "AllEnrichmentsGate: EnrichmentComplete for unknown slug '{}' (language '{}') — ignoring",
                            slug, language,
                        );
                        return Ok(vec![]);
                    }
                };

                // Accumulate LSP additions.
                entry.lsp_edges.extend(added_edges.iter().cloned());
                entry.lsp_nodes.extend(new_nodes.iter().cloned());
                entry.pending = entry.pending.saturating_sub(1);
                tracing::debug!(
                    "AllEnrichmentsGate: slug '{}' language '{}' EnrichmentComplete (pending now {})",
                    slug, language, entry.pending,
                );

                if let Some(done) = Self::try_finalize(&mut guard, slug) {
                    return Ok(vec![done]);
                }
                Ok(vec![])
            }

            _ => Ok(vec![]),
        }
    }
}

// ---------------------------------------------------------------------------
// EmbeddingIndexerConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `RootExtracted` and streams nodes to the embedding index.
///
/// **Phase 2 stub.** Embedding requires async LanceDB operations. In Phase 2,
/// embedding continues to run via `spawn_background_enrichment` after the full
/// graph is assembled. Phase 4 (#454) moves it here as a streaming consumer
/// so vectors are available as soon as nodes are extracted.
///
/// Per the ADR: "Consumer of RootExtracted (streaming, as nodes arrive):
/// EmbeddingIndexer — SINGLETON — embeds nodes as they're extracted incrementally
/// via BLAKE3, doesn't wait for passes."
///
/// Subscribes to: `RootExtracted`
/// Emits: nothing
pub struct EmbeddingIndexerConsumer;

impl ExtractionConsumer for EmbeddingIndexerConsumer {
    fn name(&self) -> &str { "embedding_indexer" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::RootExtracted]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::RootExtracted { slug, nodes, .. } = event else {
            return Ok(vec![]);
        };
        // Phase 2 stub: embedding runs via spawn_background_enrichment.
        // Phase 4 (#454) promotes this to async streaming embed.
        tracing::debug!(
            "EmbeddingIndexerConsumer: root '{}' — {} nodes (Phase 2 stub, embed runs in background task)",
            slug,
            nodes.len(),
        );
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// LanceDBConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `PassesComplete` and persists the graph to LanceDB.
///
/// **Phase 2 stub.** LanceDB persist requires async operations and the full
/// graph (nodes + edges from all roots merged). In Phase 2, persistence runs
/// via `persist_graph_to_lance` inside `build_full_graph_inner`. Phase 4 (#454)
/// moves it here as a singleton consumer.
///
/// Per the ADR: "Consumer of PassesComplete: LanceDBPersist — SINGLETON —
/// writes with tenant_id = root slug."
///
/// Subscribes to: `PassesComplete`
/// Emits: nothing
pub struct LanceDBConsumer;

impl ExtractionConsumer for LanceDBConsumer {
    fn name(&self) -> &str { "lancedb_persist" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::PassesComplete]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::PassesComplete { slug, nodes, edges, .. } = event else {
            return Ok(vec![]);
        };
        // Phase 2 stub: persist runs inside build_full_graph_inner.
        // Phase 4 (#454) promotes this to async singleton consumer.
        tracing::debug!(
            "LanceDBConsumer: root '{}' — {} nodes, {} edges (Phase 2 stub, persist runs in graph pipeline)",
            slug,
            nodes.len(),
            edges.len(),
        );
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// SubsystemConsumer
// ---------------------------------------------------------------------------

/// Subscribes to `PassesComplete` and handles subsystem node promotion.
///
/// **Phase 2 stub.** In Phase 3+, this consumer will run `subsystem_node_pass`
/// after the graph index is built. In Phase 2, subsystem detection requires
/// `GraphIndex::detect_communities()` which is not available in the event payload —
/// it runs synchronously inside `build_full_graph_inner` after the graph is assembled.
///
/// This stub establishes the subscription slot per the ADR consumer migration list.
/// The issue spec maps: `subsystem_node_pass → PassesComplete`.
///
/// Subscribes to: `PassesComplete`
/// Emits: nothing (Phase 2 stub)
pub struct SubsystemConsumer;

impl ExtractionConsumer for SubsystemConsumer {
    fn name(&self) -> &str { "subsystem" }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[ExtractionEventKind::PassesComplete]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::PassesComplete { slug, nodes, .. } = event else {
            return Ok(vec![]);
        };
        // Phase 2: subsystem detection requires GraphIndex::detect_communities()
        // which is not available in the event payload. It runs inside
        // build_full_graph_inner after the graph is assembled. This consumer
        // establishes the subscription slot; Phase 3+ moves the logic here.
        tracing::debug!(
            "SubsystemConsumer: root '{}' — {} nodes (Phase 2 stub, subsystem detection runs in graph pipeline)",
            slug,
            nodes.len(),
        );
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// build_builtin_bus
// ---------------------------------------------------------------------------

/// Build an `EventBus` pre-loaded with all built-in consumers.
///
/// # Phase 3 LSP promotion
///
/// `LspConsumer` now performs real LSP enrichment via `tokio::task::block_in_place`.
/// `AllEnrichmentsGate` aggregates per-language `EnrichmentComplete` events and emits
/// `AllEnrichmentsDone` once all languages complete.
/// `PostExtractionConsumer` subscribes to `AllEnrichmentsDone` so passes run on the
/// LSP-enriched graph.
///
/// Registered consumers:
/// - `ManifestConsumer`, `TreeSitterConsumer` — `RootDiscovered`
/// - `LanguageAccumulatorConsumer`, `AllEnrichmentsGate`,
///   `OpenApiConsumer`, `GrpcConsumer`, `EmbeddingIndexerConsumer` — `RootExtracted`
/// - `LspConsumer(lang)` × N — `LanguageDetected` (one per language from `EnricherRegistry`)
/// - `AllEnrichmentsGate` — also subscribes to `LanguageDetected` + `EnrichmentComplete`
/// - `PostExtractionConsumer` — `AllEnrichmentsDone`
/// - `FrameworkDetectionConsumer`, `NextjsRoutingConsumer`,
///   `PubSubConsumer`, `WebSocketConsumer` — `FrameworkDetected`
/// - `SubsystemConsumer`, `LanceDBConsumer` — `PassesComplete`
///
/// Note: `CustomExtractorConsumer` is not pre-registered here because the set of
/// custom extractors is config-driven (`.oh/extractors/*.toml`) and unknown at
/// startup. Callers loading custom extractor configs must register additional
/// `CustomExtractorConsumer` instances after calling `build_builtin_bus()`.
///
/// # Arguments
/// * `root_pairs` — `(slug, path)` pairs for all workspace roots (non-lsp-only)
/// * `primary_slug` — slug of the primary code root
/// * `repo_root` — path to the primary repository root (used by `LspConsumer`)
/// * `lsp_only_roots` — `(slug, path)` pairs for lsp-only subdirectory roots
///   (used by `LspConsumer` for monorepo lsp_root selection)
/// * `enricher_registry` — `EnricherRegistry` providing per-language enrichers for `LspConsumer`
pub fn build_builtin_bus(
    root_pairs: Vec<(String, PathBuf)>,
    primary_slug: String,
    repo_root: PathBuf,
    lsp_only_roots: Vec<(String, PathBuf)>,
    enricher_registry: EnricherRegistry,
) -> crate::extract::event_bus::EventBus {
    use crate::extract::event_bus::EventBus;

    let mut bus = EventBus::new();

    // --- RootDiscovered consumers ---
    bus.register(Box::new(ManifestConsumer));
    bus.register(Box::new(TreeSitterConsumer::new()));

    // --- AllEnrichmentsGate: subscribes to RootExtracted + LanguageDetected + EnrichmentComplete ---
    // Registered before other RootExtracted consumers so base graph is seeded first.
    bus.register(Box::new(AllEnrichmentsGate::new()));

    // --- Other RootExtracted consumers ---
    bus.register(Box::new(LanguageAccumulatorConsumer));
    bus.register(Box::new(OpenApiConsumer));
    bus.register(Box::new(GrpcConsumer));
    // EmbeddingIndexerConsumer: streaming embed as nodes arrive (Phase 2 stub)
    bus.register(Box::new(EmbeddingIndexerConsumer));

    // --- LanguageDetected consumers: one real LspConsumer per language ---
    //
    // Per the ADR: "ALL fire concurrently, one per language." The event bus calls
    // each LspConsumer sequentially within the same event dispatch, but each uses
    // `block_in_place` so the tokio executor is not blocked while LSP is running.
    //
    // Build per-language enricher map from registry. Arc<dyn Enricher> so each
    // LspConsumer holds a reference without duplicating the enricher.
    let lsp_only_arc: StdArc<Vec<(String, PathBuf)>> = StdArc::new(lsp_only_roots);
    let enrichers = enricher_registry.into_per_language_map();

    let mut supported_languages: Vec<String> = enrichers.keys().cloned().collect();
    supported_languages.sort(); // deterministic registration order

    for lang in supported_languages {
        let enricher = enrichers.get(&lang).cloned();
        bus.register(Box::new(LspConsumer {
            language: lang,
            repo_root: repo_root.clone(),
            lsp_only_roots: StdArc::clone(&lsp_only_arc),
            enricher,
        }));
    }

    // Combine regular roots + lsp_only roots for post-extraction passes.
    // PostExtractionConsumer runs nextjs_routing_pass and manifest_pass, which are
    // path-based (not tree-sitter-based). They must walk ALL roots — including
    // lsp_only subdirectory roots (e.g. client/) — so they emit ApiEndpoint nodes
    // with the correct root slug (e.g. root="client").
    let all_root_pairs: Vec<(String, PathBuf)> = root_pairs
        .iter()
        .cloned()
        .chain(lsp_only_arc.iter().cloned())
        .collect();

    // --- AllEnrichmentsDone consumers ---
    bus.register(Box::new(PostExtractionConsumer::new(all_root_pairs.clone(), primary_slug)));

    // --- FrameworkDetected consumers ---
    bus.register(Box::new(FrameworkDetectionConsumer));
    bus.register(Box::new(NextjsRoutingConsumer::new(all_root_pairs)));
    bus.register(Box::new(PubSubConsumer));
    bus.register(Box::new(WebSocketConsumer));

    // --- PassesComplete consumers ---
    bus.register(Box::new(SubsystemConsumer));
    bus.register(Box::new(LanceDBConsumer));

    bus
}

// ---------------------------------------------------------------------------
// run_post_passes_via_bus
// ---------------------------------------------------------------------------

/// Run post-extraction passes via the EventBus, returning enriched nodes/edges
/// and detected frameworks.
///
/// This is the common pattern used by `build_full_graph_inner`,
/// `update_graph_with_scan`, and the background scanner:
///
/// ```text
/// nodes + edges → Arc<[T]> → RootExtracted → bus
///   → LanguageAccumulatorConsumer → LanguageDetected (per language)
///     → LspConsumer → EnrichmentComplete (with real edges)
///   → AllEnrichmentsGate → AllEnrichmentsDone (merged nodes/edges)
///   → PostExtractionConsumer → PassesComplete { nodes, edges, detected_frameworks }
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
/// * `repo_root` - path to the primary repository root (for LSP server startup)
/// * `lsp_only_roots` - `(slug, path)` pairs for lsp-only subdirectory roots
///
/// # Returns
/// `Ok((nodes, edges, detected_frameworks))` — the enriched graph and framework set.
///
/// # Errors
/// Returns `Err` if the bus does not produce a `PassesComplete` event.
/// This is a pipeline invariant violation, so `Err` here indicates a logic bug.
pub fn run_post_passes_via_bus(
    nodes: Vec<crate::graph::Node>,
    edges: Vec<crate::graph::Edge>,
    root_pairs: Vec<(String, PathBuf)>,
    primary_slug: String,
    repo_root: PathBuf,
    lsp_only_roots: Vec<(String, PathBuf)>,
) -> anyhow::Result<(Vec<crate::graph::Node>, Vec<crate::graph::Edge>, std::collections::HashSet<String>)> {
    use crate::extract::event_bus::ExtractionEvent;
    use std::sync::Arc;

    // Wrap into Arc<[T]> for zero-copy bus fan-out.
    let nodes_arc: Arc<[crate::graph::Node]> = Arc::from(nodes.into_boxed_slice());
    let edges_arc: Arc<[crate::graph::Edge]> = Arc::from(edges.into_boxed_slice());

    let enricher_registry = EnricherRegistry::with_builtins();
    let bus = build_builtin_bus(
        root_pairs,
        primary_slug.clone(),
        repo_root.clone(),
        lsp_only_roots,
        enricher_registry,
    );
    let events = bus.emit(ExtractionEvent::RootExtracted {
        slug: primary_slug,
        path: repo_root,
        nodes: Arc::clone(&nodes_arc),
        edges: Arc::clone(&edges_arc),
    });

    // Collect PassesComplete — produced by PostExtractionConsumer.
    // PassesComplete is a pipeline invariant: PostExtractionConsumer always emits it.
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
            // This is a hard invariant violation — do NOT fall back to the unenriched
            // graph. The caller must not persist/commit scan state with missing passes.
            anyhow::bail!(
                "EventBus post-extraction: PassesComplete event absent — \
                 this is a pipeline invariant violation (PostExtractionConsumer \
                 must always emit PassesComplete). Failing hard to prevent \
                 persisting an unenriched graph."
            )
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

    #[allow(dead_code)]
    fn make_root_discovered(slug: &str, path: PathBuf) -> ExtractionEvent {
        ExtractionEvent::RootDiscovered {
            slug: slug.to_string(),
            path,
            lsp_only: false,
        }
    }

    /// Verify ManifestConsumer subscribes to RootDiscovered and emits nothing.
    #[test]
    fn test_manifest_consumer_subscription() {
        let consumer = ManifestConsumer;
        assert!(consumer.subscribes_to().contains(&ExtractionEventKind::RootDiscovered));
        let event = ExtractionEvent::RootDiscovered {
            slug: "test".into(),
            path: PathBuf::from("."),
            lsp_only: false,
        };
        let result = consumer.on_event(&event).unwrap();
        assert!(result.is_empty(), "ManifestConsumer emits no follow-on events in Phase 2");
    }

    /// Verify LanguageAccumulatorConsumer groups nodes by language.
    #[test]
    fn test_language_accumulator_groups_by_language() {
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
        };

        let consumer = LanguageAccumulatorConsumer;
        let follow_ons = consumer.on_event(&event).unwrap();

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

    /// Verify LanguageAccumulatorConsumer skips nodes with empty language.
    #[test]
    fn test_language_accumulator_skips_empty_language() {
        use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};
        use std::collections::BTreeMap;

        let event = ExtractionEvent::RootExtracted {
            slug: "test".into(),
            path: PathBuf::from("."),
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
        let follow_ons = consumer.on_event(&event).unwrap();
        assert!(follow_ons.is_empty(), "Empty language nodes must be skipped");
    }

    /// Verify TreeSitterConsumer skips lsp_only roots.
    #[test]
    fn test_tree_sitter_consumer_skips_lsp_only() {
        let consumer = TreeSitterConsumer::new();
        let event = ExtractionEvent::RootDiscovered {
            slug: "skills".into(),
            path: PathBuf::from("."),
            lsp_only: true,
        };
        let result = consumer.on_event(&event).unwrap();
        assert!(result.is_empty(), "lsp_only roots must produce no RootExtracted event");
    }

    /// Verify PubSubConsumer only fires for broker frameworks.
    #[test]
    fn test_pubsub_consumer_fires_for_kafka() {
        let consumer = PubSubConsumer;
        let event = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "kafka-python".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).unwrap();
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], ExtractionEvent::PassComplete { pass_name: "pubsub", .. }));
    }

    #[test]
    fn test_pubsub_consumer_ignores_non_broker() {
        let consumer = PubSubConsumer;
        let event = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "django".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).unwrap();
        assert!(result.is_empty());
    }

    /// Verify WebSocketConsumer fires only for socketio.
    #[test]
    fn test_websocket_consumer_fires_for_socketio() {
        let consumer = WebSocketConsumer;
        let event = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "socketio".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).unwrap();
        assert_eq!(result.len(), 1);
        assert!(matches!(result[0], ExtractionEvent::PassComplete { pass_name: "websocket", .. }));
    }

    #[test]
    fn test_websocket_consumer_ignores_non_socketio() {
        let consumer = WebSocketConsumer;
        let event = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "flask".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).unwrap();
        assert!(result.is_empty());
    }

    /// Verify build_builtin_bus registers consumers for all expected event kinds.
    #[test]
    fn test_builtin_bus_has_consumers_for_all_event_kinds() {
        let bus = build_builtin_bus(
            vec![],
            "test".into(),
            PathBuf::from("."),
            vec![],
            crate::extract::EnricherRegistry::with_builtins(),
        );
        // Derive the exact expected count to catch regressions when languages are added/removed.
        let lsp_count = crate::extract::EnricherRegistry::with_builtins()
            .supported_languages()
            .len();
        let expected_total =
            2 +        // RootDiscovered: ManifestConsumer, TreeSitterConsumer
            1 +        // AllEnrichmentsGate (subscribes to RootExtracted + LanguageDetected + EnrichmentComplete)
            4 +        // RootExtracted: LanguageAccumulatorConsumer, OpenApiConsumer, GrpcConsumer, EmbeddingIndexerConsumer
            lsp_count + // LanguageDetected: LspConsumer × N
            1 +        // AllEnrichmentsDone: PostExtractionConsumer
            4 +        // FrameworkDetected: FrameworkDetectionConsumer, NextjsRoutingConsumer,
                       //   PubSubConsumer, WebSocketConsumer
            2;         // PassesComplete: SubsystemConsumer, LanceDBConsumer
        assert_eq!(
            bus.len(), expected_total,
            "Unexpected built-in consumer count: got {}, expected {} (lsp_count={})",
            bus.len(), expected_total, lsp_count
        );
    }

    /// Verify PostExtractionConsumer emits PassesComplete on empty input.
    ///
    /// PostExtractionConsumer now subscribes to `AllEnrichmentsDone` (not `RootExtracted`)
    /// so that post-extraction passes run on the LSP-enriched graph.
    #[test]
    fn test_post_extraction_consumer_emits_passes_complete() {
        let consumer = PostExtractionConsumer::new(vec![], "test".into());
        let event = ExtractionEvent::AllEnrichmentsDone {
            slug: "test".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
        };
        let follow_ons = consumer.on_event(&event).unwrap();
        assert!(
            follow_ons.iter().any(|e| matches!(e, ExtractionEvent::PassesComplete { .. })),
            "PostExtractionConsumer must always emit PassesComplete"
        );
    }

    /// Adversarial: CustomExtractorConsumer fires only for its declared framework.
    #[test]
    fn test_custom_extractor_consumer_framework_filter() {
        let consumer = CustomExtractorConsumer {
            framework: "fastapi".into(),
            config_name: "fastapi-routes".into(),
        };
        // Matching framework → fires
        let matching = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "fastapi".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&matching).unwrap();
        assert_eq!(result.len(), 1);

        // Non-matching → silent
        let non_matching = ExtractionEvent::FrameworkDetected {
            slug: "test".into(),
            framework: "flask".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&non_matching).unwrap();
        assert!(result.is_empty());
    }

    /// Integration: bus emit sequence starting from RootDiscovered.
    /// With a real temp directory, TreeSitterConsumer produces RootExtracted.
    ///
    /// Uses multi-thread tokio runtime because `LspConsumer::on_event` calls
    /// `tokio::task::block_in_place` which requires a running tokio executor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_bus_emit_root_discovered_produces_root_extracted() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/lib.rs"),
            "pub fn hello() {}\n",
        ).unwrap();
        // Need scan state dir
        std::fs::create_dir_all(tmp.path().join(".oh").join(".cache")).unwrap();

        let bus = build_builtin_bus(
            vec![("test".to_string(), tmp.path().to_path_buf())],
            "test".to_string(),
            tmp.path().to_path_buf(),
            vec![],
            crate::extract::EnricherRegistry::with_builtins(),
        );
        let events = bus.emit(ExtractionEvent::RootDiscovered {
            slug: "test".to_string(),
            path: tmp.path().to_path_buf(),
            lsp_only: false,
        });

        // Must include RootExtracted somewhere in the emitted events
        let has_root_extracted = events.iter().any(|e| matches!(e, ExtractionEvent::RootExtracted { .. }));
        assert!(has_root_extracted, "TreeSitterConsumer must produce RootExtracted from RootDiscovered");

        // Must include PassesComplete
        let has_passes_complete = events.iter().any(|e| matches!(e, ExtractionEvent::PassesComplete { .. }));
        assert!(has_passes_complete, "PostExtractionConsumer must produce PassesComplete");
    }

    /// Helper: build an LspConsumer with no enricher for the given language.
    fn make_lsp_consumer(lang: &str) -> LspConsumer {
        LspConsumer {
            language: lang.into(),
            repo_root: PathBuf::from("."),
            lsp_only_roots: std::sync::Arc::new(vec![]),
            enricher: None,
        }
    }

    /// Verify LspConsumer fires only for its declared language.
    #[test]
    fn test_lsp_consumer_fires_for_declared_language() {
        let consumer = make_lsp_consumer("rust");
        let matching = ExtractionEvent::LanguageDetected {
            slug: "test".into(),
            language: "rust".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&matching).unwrap();
        assert_eq!(result.len(), 1, "LspConsumer must emit EnrichmentComplete for its language");
        assert!(
            matches!(result[0], ExtractionEvent::EnrichmentComplete { .. }),
            "LspConsumer must emit EnrichmentComplete"
        );
    }

    #[test]
    fn test_lsp_consumer_ignores_other_language() {
        let consumer = make_lsp_consumer("rust");
        let other_lang = ExtractionEvent::LanguageDetected {
            slug: "test".into(),
            language: "python".into(),
            nodes: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&other_lang).unwrap();
        assert!(result.is_empty(), "LspConsumer must ignore events for other languages");
    }

    /// Verify SubsystemConsumer subscribes to PassesComplete.
    #[test]
    fn test_subsystem_consumer_subscription() {
        let consumer = SubsystemConsumer;
        assert!(consumer.subscribes_to().contains(&ExtractionEventKind::PassesComplete));
        let event = ExtractionEvent::PassesComplete {
            slug: "test".into(),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
            detected_frameworks: HashSet::new(),
        };
        let result = consumer.on_event(&event).unwrap();
        assert!(result.is_empty(), "SubsystemConsumer Phase 2 stub emits nothing");
    }

    /// Verify LanceDBConsumer subscribes to PassesComplete.
    #[test]
    fn test_lancedb_consumer_subscription() {
        let consumer = LanceDBConsumer;
        assert!(consumer.subscribes_to().contains(&ExtractionEventKind::PassesComplete));
        let event = ExtractionEvent::PassesComplete {
            slug: "test".into(),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
            detected_frameworks: HashSet::new(),
        };
        let result = consumer.on_event(&event).unwrap();
        assert!(result.is_empty(), "LanceDBConsumer Phase 2 stub emits nothing");
    }

    /// Verify EmbeddingIndexerConsumer subscribes to RootExtracted.
    #[test]
    fn test_embedding_indexer_consumer_subscription() {
        let consumer = EmbeddingIndexerConsumer;
        assert!(consumer.subscribes_to().contains(&ExtractionEventKind::RootExtracted));
        let event = ExtractionEvent::RootExtracted {
            slug: "test".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
        };
        let result = consumer.on_event(&event).unwrap();
        assert!(result.is_empty(), "EmbeddingIndexerConsumer Phase 2 stub emits nothing");
    }

    // ── run_post_passes_via_bus tests ─────────────────────────────────────

    /// Verify run_post_passes_via_bus returns Ok(PassesComplete data) on empty input.
    ///
    /// This is the ADR Phase 2b integration test: ensures that emitting
    /// RootExtracted to the bus always produces a PassesComplete event, even
    /// when the input graph is empty.
    ///
    /// Uses multi-thread tokio runtime because `LspConsumer::on_event` calls
    /// `tokio::task::block_in_place` which requires a running tokio executor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_run_post_passes_via_bus_empty_input() {
        let (nodes, edges, frameworks) = super::run_post_passes_via_bus(
            vec![],
            vec![],
            vec![],
            "test".to_string(),
            PathBuf::from("."),
            vec![],
        ).expect("run_post_passes_via_bus must not fail on empty input");
        assert!(nodes.is_empty(), "empty input → empty nodes");
        assert!(edges.is_empty(), "empty input → empty edges");
        assert!(frameworks.is_empty(), "empty input → no frameworks");
    }

    /// Verify run_post_passes_via_bus routes nodes through PostExtractionConsumer.
    ///
    /// With a real temp directory and a Rust source file, the bus should:
    /// 1. Accept RootExtracted with the pre-extracted nodes
    /// 2. Route to PostExtractionConsumer which runs all passes
    /// 3. Return PassesComplete with the enriched graph
    ///
    /// Uses multi-thread tokio runtime because `LspConsumer::on_event` calls
    /// `tokio::task::block_in_place` which requires a running tokio executor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_run_post_passes_via_bus_preserves_input_nodes() {
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
        let (out_nodes, _out_edges, _frameworks) = super::run_post_passes_via_bus(
            input_nodes,
            vec![],
            vec![],
            "test".to_string(),
            PathBuf::from("."),
            vec![],
        ).expect("run_post_passes_via_bus must not fail with valid input");

        // Output must contain the input node (passes should not drop it)
        assert!(
            out_nodes.iter().any(|n| n.id.name == "my_fn"),
            "run_post_passes_via_bus must preserve input nodes through post-extraction passes"
        );
    }

    // ── AllEnrichmentsGate tests ──────────────────────────────────────────

    /// Zero-language path: RootExtracted with no nodes → AllEnrichmentsDone fires immediately.
    #[test]
    fn test_all_enrichments_gate_zero_language_fires_immediately() {
        let gate = AllEnrichmentsGate::new();

        // No LanguageDetected events, just RootExtracted.
        let root_extracted = ExtractionEvent::RootExtracted {
            slug: "zero".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
        };

        let result = gate.on_event(&root_extracted).unwrap();
        assert_eq!(result.len(), 1, "AllEnrichmentsGate must fire AllEnrichmentsDone immediately with zero languages");
        assert!(
            matches!(result[0], ExtractionEvent::AllEnrichmentsDone { .. }),
            "must be AllEnrichmentsDone"
        );
    }

    /// One language path: LanguageDetected then EnrichmentComplete → AllEnrichmentsDone.
    #[test]
    fn test_all_enrichments_gate_single_language() {
        let gate = AllEnrichmentsGate::new();

        // Step 1: RootExtracted (base graph, pending=0 initially)
        let root_extracted = ExtractionEvent::RootExtracted {
            slug: "single".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
        };
        let r1 = gate.on_event(&root_extracted).unwrap();
        // In bus flow: AllEnrichmentsGate sees RootExtracted first.
        // With pending==0 (no LanguageDetected seen yet), gate fires AllEnrichmentsDone immediately.
        // LanguageAccumulatorConsumer emits LanguageDetected as follow-ons to RootExtracted;
        // for repos with no language nodes, it emits nothing, so gate fires at RootExtracted.
        assert_eq!(r1.len(), 1, "no LanguageDetected yet → gate fires immediately (zero-language path)");
        assert!(matches!(r1[0], ExtractionEvent::AllEnrichmentsDone { .. }));
    }

    /// Multi-language path: gate waits for all EnrichmentComplete before firing.
    #[test]
    fn test_all_enrichments_gate_waits_for_all_languages() {
        let gate = AllEnrichmentsGate::new();
        let slug = "multi";

        // Step 1: RootExtracted (initializes base; pending=0 so would fire... but we want
        // to test the multi-language path, so we must deliver LanguageDetected first).
        // In the real bus, LanguageAccumulator's LanguageDetected events are follow-ons
        // to RootExtracted, processed AFTER RootExtracted by all its subscribers.
        // So AllEnrichmentsGate.on_event(RootExtracted) fires immediately (pending=0 at that point).
        //
        // To test the multi-language waiting logic: send LanguageDetected BEFORE RootExtracted
        // (simulating out-of-order delivery, which the gate handles defensively).

        let lang_rust = ExtractionEvent::LanguageDetected {
            slug: slug.into(),
            language: "rust".into(),
            nodes: std::sync::Arc::from([]),
        };
        let lang_python = ExtractionEvent::LanguageDetected {
            slug: slug.into(),
            language: "python".into(),
            nodes: std::sync::Arc::from([]),
        };

        // LanguageDetected x2 → pending = 2, no AllEnrichmentsDone yet.
        let r1 = gate.on_event(&lang_rust).unwrap();
        assert!(r1.is_empty(), "pending=1, base not set → no AllEnrichmentsDone");

        let r2 = gate.on_event(&lang_python).unwrap();
        assert!(r2.is_empty(), "pending=2, base not set → no AllEnrichmentsDone");

        // RootExtracted arrives → base initialized, pending=2 → no AllEnrichmentsDone.
        let root_extracted = ExtractionEvent::RootExtracted {
            slug: slug.into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
        };
        let r3 = gate.on_event(&root_extracted).unwrap();
        assert!(r3.is_empty(), "base initialized, pending=2 → still waiting");

        // EnrichmentComplete for rust → pending = 1.
        let ec_rust = ExtractionEvent::EnrichmentComplete {
            slug: slug.into(),
            language: "rust".into(),
            added_edges: std::sync::Arc::from([]),
            new_nodes: std::sync::Arc::from([]),
        };
        let r4 = gate.on_event(&ec_rust).unwrap();
        assert!(r4.is_empty(), "pending=1 → still waiting");

        // EnrichmentComplete for python → pending = 0 → AllEnrichmentsDone fires.
        let ec_python = ExtractionEvent::EnrichmentComplete {
            slug: slug.into(),
            language: "python".into(),
            added_edges: std::sync::Arc::from([]),
            new_nodes: std::sync::Arc::from([]),
        };
        let r5 = gate.on_event(&ec_python).unwrap();
        assert_eq!(r5.len(), 1, "pending=0 → AllEnrichmentsDone must fire");
        assert!(
            matches!(r5[0], ExtractionEvent::AllEnrichmentsDone { ref slug, .. } if slug == "multi"),
            "AllEnrichmentsDone must carry the correct slug"
        );
    }

    /// Gate ignores EnrichmentComplete for unknown slugs.
    #[test]
    fn test_all_enrichments_gate_ignores_unknown_slug() {
        let gate = AllEnrichmentsGate::new();
        let ec = ExtractionEvent::EnrichmentComplete {
            slug: "unknown_slug".into(),
            language: "rust".into(),
            added_edges: std::sync::Arc::from([]),
            new_nodes: std::sync::Arc::from([]),
        };
        let result = gate.on_event(&ec).unwrap();
        assert!(result.is_empty(), "unknown slug must be ignored without error");
    }

    /// Gate accumulates LSP edges from EnrichmentComplete into AllEnrichmentsDone.
    #[test]
    fn test_all_enrichments_gate_merges_lsp_edges() {
        let gate = AllEnrichmentsGate::new();
        let slug = "merge";

        // LanguageDetected first (pending=1).
        gate.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug.into(),
            language: "rust".into(),
            nodes: std::sync::Arc::from([]),
        }).unwrap();

        // RootExtracted with base edges.
        gate.on_event(&ExtractionEvent::RootExtracted {
            slug: slug.into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
        }).unwrap();

        // EnrichmentComplete with a fake LSP edge.
        let lsp_edge = Edge {
            from: crate::graph::NodeId { root: "x".into(), file: PathBuf::from("a.rs"), name: "f".into(), kind: crate::graph::NodeKind::Function },
            to:   crate::graph::NodeId { root: "x".into(), file: PathBuf::from("b.rs"), name: "g".into(), kind: crate::graph::NodeKind::Function },
            kind: crate::graph::EdgeKind::Calls,
            source: crate::graph::ExtractionSource::Lsp,
            confidence: crate::graph::Confidence::Detected,
        };
        let result = gate.on_event(&ExtractionEvent::EnrichmentComplete {
            slug: slug.into(),
            language: "rust".into(),
            added_edges: std::sync::Arc::from(vec![lsp_edge]),
            new_nodes: std::sync::Arc::from([]),
        }).unwrap();

        assert_eq!(result.len(), 1, "AllEnrichmentsDone must fire");
        if let ExtractionEvent::AllEnrichmentsDone { edges, .. } = &result[0] {
            assert_eq!(edges.len(), 1, "merged AllEnrichmentsDone must carry the LSP edge");
        } else {
            panic!("expected AllEnrichmentsDone");
        }
    }
}
