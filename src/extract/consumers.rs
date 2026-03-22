//! Built-in `ExtractionConsumer` implementations.
//!
//! Each consumer wraps an existing extraction pass and subscribes to the
//! appropriate event. No consumer imports or calls another consumer.
//!
//! # Registration order
//!
//! Consumers must be registered in the order shown in `EventBus::with_builtins()`.
//! The ordering invariant:
//!
//! 1. `ManifestConsumer` — subscribes to `RootDiscovered` (needs filesystem)
//! 2. `TreeSitterConsumer` — subscribes to `RootDiscovered`, emits `RootExtracted`
//! 3. `LanguageAccumulatorConsumer` — subscribes to `RootExtracted`, emits `LanguageDetected`
//! 4. `PostExtractionConsumer` — subscribes to `RootExtracted`, runs all post-extraction passes,
//!    emits `PassesComplete` (and `FrameworkDetected` for each detected framework)
//! 5. `EmbeddingConsumer` — subscribes to `RootExtracted` (streaming embed as nodes arrive)
//!
//! The `LspConsumer` and persistence consumers (`SubsystemConsumer`, `LanceDBConsumer`) are
//! wired separately by the pipeline bootstrap because they require async execution.
//! In Phase 2 they continue to run via the existing spawn_background_enrichment path;
//! the event bus handles the synchronous portion of the pipeline.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::extract::ExtractorRegistry;
use crate::extract::event_bus::{ExtractionConsumer, ExtractionEvent, ExtractionEventKind};
use crate::graph::Node;

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

/// Runs all post-extraction passes (via `PostExtractionRegistry`) after tree-sitter extraction.
///
/// Subscribes to: `RootExtracted`
/// Emits: `FrameworkDetected` (one per detected framework) + `PassesComplete`
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
        &[ExtractionEventKind::RootExtracted]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let ExtractionEvent::RootExtracted { slug, nodes, edges, .. } = event else {
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

/// Subscribes to `LanguageDetected` and runs LSP enrichment for the given language.
///
/// **Phase 2 stub.** LSP enrichers require async execution — they start language
/// servers and communicate via JSON-RPC, which requires tokio. The event bus in
/// Phase 2 is synchronous. LSP enrichment continues to run via the existing
/// `spawn_background_enrichment` path.
///
/// In Phase 3, the bus will gain async support and this consumer will call
/// `EnricherRegistry::enrich_all()` for the specific language.
///
/// Per the ADR: "Consumer of LanguageDetected(lang, nodes): LSP enrichers — ALL fire
/// concurrently, one per language." Parallel LSP will fall out of the architecture when
/// this stub is promoted to a full async consumer.
///
/// Subscribes to: `LanguageDetected`
/// Emits: `EnrichmentComplete` (Phase 2 stub — no-op)
pub struct LspConsumer {
    /// Which language this consumer handles (e.g., "rust", "python", "typescript").
    pub language: String,
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
        // Phase 2 stub: log that LSP would run, emit EnrichmentComplete with no edges.
        // In Phase 3+: start the language server and run enrich() asynchronously.
        tracing::debug!(
            "LspConsumer({}): root '{}' language '{}' — {} nodes (Phase 2 stub, async LSP not yet wired)",
            self.language,
            slug,
            language,
            nodes.len(),
        );
        Ok(vec![ExtractionEvent::EnrichmentComplete {
            slug: slug.clone(),
            language: language.clone(),
            added_edges: std::sync::Arc::from([]),
            new_nodes: std::sync::Arc::from([]),
        }])
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
// EventBus::with_builtins
// ---------------------------------------------------------------------------

/// Build an `EventBus` pre-loaded with all built-in consumers.
///
/// All consumers — including async ones (LSP, embedding, LanceDB) — are registered
/// here as **Phase 2 stubs**. The stubs subscribe to the correct events and emit
/// the correct follow-on events, but do not perform the actual async work yet.
/// In Phase 2, async work continues via the existing `spawn_background_enrichment`
/// path; Phase 3+ promotes these stubs to full async consumers.
///
/// Registered consumers:
/// - `ManifestConsumer`, `TreeSitterConsumer` — `RootDiscovered`
/// - `LanguageAccumulatorConsumer`, `PostExtractionConsumer`,
///   `OpenApiConsumer`, `GrpcConsumer`, `EmbeddingIndexerConsumer` — `RootExtracted`
/// - `LspConsumer(lang)` × N — `LanguageDetected` (one per language from `EnricherRegistry`)
/// - `FrameworkDetectionConsumer`, `NextjsRoutingConsumer`,
///   `PubSubConsumer`, `WebSocketConsumer` — `FrameworkDetected`
/// - `SubsystemConsumer`, `LanceDBConsumer` — `PassesComplete`
///
/// Note: `CustomExtractorConsumer` is not pre-registered here because the set of
/// custom extractors is config-driven (`.oh/extractors/*.toml`) and unknown at
/// startup. Callers loading custom extractor configs must register additional
/// `CustomExtractorConsumer` instances after calling `build_builtin_bus()`.
///
/// `root_pairs` must be the full `(slug, path)` list for the workspace.
/// `primary_slug` is the slug of the primary code root.
pub fn build_builtin_bus(
    root_pairs: Vec<(String, PathBuf)>,
    primary_slug: String,
) -> crate::extract::event_bus::EventBus {
    use crate::extract::event_bus::EventBus;

    let mut bus = EventBus::new();

    // --- RootDiscovered consumers ---
    bus.register(Box::new(ManifestConsumer));
    bus.register(Box::new(TreeSitterConsumer::new()));

    // --- RootExtracted consumers ---
    bus.register(Box::new(LanguageAccumulatorConsumer));
    bus.register(Box::new(PostExtractionConsumer::new(root_pairs.clone(), primary_slug)));
    bus.register(Box::new(OpenApiConsumer));
    bus.register(Box::new(GrpcConsumer));
    // EmbeddingIndexerConsumer: streaming embed as nodes arrive (Phase 2 stub)
    bus.register(Box::new(EmbeddingIndexerConsumer));

    // --- LanguageDetected consumers (one LSP stub per language) ---
    // Per the ADR: "ALL fire concurrently, one per language" — in Phase 3+ these
    // will be promoted to async consumers running in parallel.
    //
    // Derive from EnricherRegistry so this list never drifts when languages
    // are added or removed from the enrichment stack.
    let mut supported_languages: Vec<String> = crate::extract::EnricherRegistry::with_builtins()
        .supported_languages()
        .into_iter()
        .collect();
    supported_languages.sort(); // deterministic registration order
    for lang in supported_languages {
        bus.register(Box::new(LspConsumer { language: lang }));
    }

    // --- FrameworkDetected consumers ---
    bus.register(Box::new(FrameworkDetectionConsumer));
    bus.register(Box::new(NextjsRoutingConsumer::new(root_pairs)));
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
/// nodes + edges → Arc<[T]> → RootExtracted → bus → PostExtractionConsumer
///     → PassesComplete { nodes, edges, detected_frameworks }
/// ```
///
/// The `nodes` and `edges` arguments are consumed (moved into `Arc<[T]>` for
/// zero-copy fan-out to the bus consumers). If the bus fails to produce a
/// `PassesComplete` event, the original data is returned unchanged via the
/// fallback `Arc<[T]>` references.
///
/// # Arguments
/// * `nodes` - extracted node set (consumed)
/// * `edges` - extracted edge set (consumed)
/// * `root_pairs` - `(slug, path)` pairs for all workspace roots
/// * `primary_slug` - slug of the primary code root
/// * `repo_root` - path to the primary repository root (for the bus event)
///
/// # Returns
/// `(nodes, edges, detected_frameworks)` — the enriched graph and framework set.
pub fn run_post_passes_via_bus(
    nodes: Vec<crate::graph::Node>,
    edges: Vec<crate::graph::Edge>,
    root_pairs: Vec<(String, PathBuf)>,
    primary_slug: String,
    repo_root: PathBuf,
) -> (Vec<crate::graph::Node>, Vec<crate::graph::Edge>, std::collections::HashSet<String>) {
    use crate::extract::event_bus::ExtractionEvent;
    use std::sync::Arc;

    // Wrap into Arc<[T]> for zero-copy bus fan-out.
    // Retain references as fallback if PassesComplete is absent.
    let nodes_arc: Arc<[crate::graph::Node]> = Arc::from(nodes.into_boxed_slice());
    let edges_arc: Arc<[crate::graph::Edge]> = Arc::from(edges.into_boxed_slice());

    let bus = build_builtin_bus(root_pairs, primary_slug.clone());
    let events = bus.emit(ExtractionEvent::RootExtracted {
        slug: primary_slug,
        path: repo_root,
        nodes: Arc::clone(&nodes_arc),
        edges: Arc::clone(&edges_arc),
    });

    // Collect PassesComplete — produced by PostExtractionConsumer.
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
            (nodes.to_vec(), edges.to_vec(), detected_frameworks)
        }
        _ => {
            tracing::warn!(
                "run_post_passes_via_bus: no PassesComplete from bus — \
                 returning unenriched graph"
            );
            (nodes_arc.to_vec(), edges_arc.to_vec(), std::collections::HashSet::new())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::event_bus::{EventBus, ExtractionEvent, ExtractionEventKind};
    use std::path::PathBuf;
    use tempfile::TempDir;

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
        let bus = build_builtin_bus(vec![], "test".into());
        // Derive the exact expected count to catch regressions when languages are added/removed.
        let lsp_count = crate::extract::EnricherRegistry::with_builtins()
            .supported_languages()
            .len();
        let expected_total =
            2 +        // RootDiscovered: ManifestConsumer, TreeSitterConsumer
            5 +        // RootExtracted: LanguageAccumulatorConsumer, PostExtractionConsumer,
                       //   OpenApiConsumer, GrpcConsumer, EmbeddingIndexerConsumer
            lsp_count + // LanguageDetected: LspConsumer × N
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
    #[test]
    fn test_post_extraction_consumer_emits_passes_complete() {
        let consumer = PostExtractionConsumer::new(vec![], "test".into());
        let event = ExtractionEvent::RootExtracted {
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
    #[test]
    fn test_bus_emit_root_discovered_produces_root_extracted() {
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

    /// Verify LspConsumer fires only for its declared language.
    #[test]
    fn test_lsp_consumer_fires_for_declared_language() {
        let consumer = LspConsumer { language: "rust".into() };
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
        let consumer = LspConsumer { language: "rust".into() };
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

    /// Verify run_post_passes_via_bus returns PassesComplete data on empty input.
    ///
    /// This is the ADR Phase 2b integration test: ensures that emitting
    /// RootExtracted to the bus always produces a PassesComplete event, even
    /// when the input graph is empty.
    #[test]
    fn test_run_post_passes_via_bus_empty_input() {
        let (nodes, edges, frameworks) = super::run_post_passes_via_bus(
            vec![],
            vec![],
            vec![],
            "test".to_string(),
            PathBuf::from("."),
        );
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
    #[test]
    fn test_run_post_passes_via_bus_preserves_input_nodes() {
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
        );

        // Output must contain the input node (passes should not drop it)
        assert!(
            out_nodes.iter().any(|n| n.id.name == "my_fn"),
            "run_post_passes_via_bus must preserve input nodes through post-extraction passes"
        );
    }
}
