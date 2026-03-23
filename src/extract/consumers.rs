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
//! 6. `PostExtractionConsumer` — subscribes to `AllEnrichmentsDone`, runs all post-extraction
//!    passes on LSP-enriched nodes, emits `PassesComplete` (and `FrameworkDetected` per framework)
//! 7. `EmbeddingIndexerConsumer` — subscribes to `RootExtracted` (streaming embed as nodes arrive)
//!
//! The persistence consumers (`SubsystemConsumer`, `LanceDBConsumer`) subscribe to `PassesComplete`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

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

/// Runs all post-extraction passes (via `PostExtractionRegistry`) after LSP enrichment.
///
/// Subscribes to: `AllEnrichmentsDone`
/// Emits: `FrameworkDetected` (one per detected framework) + `PassesComplete`
///
/// **Phase 3 promotion**: previously subscribed to `RootExtracted` (Phase 2), which meant
/// post-extraction passes ran on un-enriched nodes (before LSP edges were available).
/// Now subscribes to `AllEnrichmentsDone` so passes see the full LSP-enriched graph.
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
        let ExtractionEvent::AllEnrichmentsDone { slug, nodes, edges, lsp_edges, lsp_nodes, updated_nodes } = event else {
            return Ok(vec![]);
        };

        // Merge tree-sitter nodes/edges with LSP-enriched additions before running passes.
        // This ensures post-extraction passes (api_link, manifest, tested_by, etc.) operate
        // on the full LSP-enriched graph rather than just the tree-sitter snapshot.
        let mut merged_nodes = nodes.to_vec();
        merged_nodes.extend_from_slice(lsp_nodes);
        let mut merged_edges = edges.to_vec();
        merged_edges.extend_from_slice(lsp_edges);

        // Apply metadata patches from LSP enrichment (e.g., inferred types from inlay hints).
        // The patches reference nodes by stable_id and may target tree-sitter nodes not present
        // in `lsp_nodes`. Apply them to the merged set so post-extraction passes see updated metadata.
        if !updated_nodes.is_empty() {
            // Build a stable_id → index map once (O(n)) so each patch lookup is O(1)
            // rather than O(n), avoiding O(patches × nodes) on the hot post-pass path.
            let node_pos: std::collections::HashMap<String, usize> = merged_nodes
                .iter()
                .enumerate()
                .map(|(i, n)| (n.stable_id(), i))
                .collect();
            for (node_id, patches) in updated_nodes.iter() {
                if let Some(&idx) = node_pos.get(node_id) {
                    let node = &mut merged_nodes[idx];
                    for (key, value) in patches {
                        node.metadata.insert(key.clone(), value.clone());
                    }
                }
            }
            tracing::debug!(
                "PostExtractionConsumer: root '{}' applied {} metadata patch(es) from LSP enrichment",
                slug,
                updated_nodes.len(),
            );
        }

        // Delegate to the inner pass logic with merged nodes/edges.
        self.run_passes(slug, merged_nodes, merged_edges)
    }
}

impl PostExtractionConsumer {
    fn run_passes(
        &self,
        slug: &str,
        nodes_vec: Vec<Node>,
        edges_vec: Vec<crate::graph::Edge>,
    ) -> anyhow::Result<Vec<ExtractionEvent>> {
        use crate::extract::post_extraction::{PassContext, PostExtractionRegistry};

        let registry = PostExtractionRegistry::with_builtins();
        let ctx = PassContext {
            root_pairs: self.root_pairs.clone(),
            primary_slug: self.primary_slug.clone(),
            detected_frameworks: HashSet::new(),
        };

        // PostExtractionRegistry::run_all takes mutable Vec references and appends
        // items. We pass in the merged (tree-sitter + LSP) nodes/edges from the caller.
        // The resulting all_nodes/all_edges are wrapped back into Arc<[T]> for the
        // PassesComplete event so downstream consumers share the same allocation.
        let mut all_nodes = nodes_vec;
        let mut all_edges = edges_vec;
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
                slug: slug.to_string(),
                framework: framework.clone(),
                nodes: std::sync::Arc::from(fw_nodes.into_boxed_slice()),
            });
        }

        // Always emit PassesComplete with the enriched graph.
        // Wrap into Arc<[T]> so all PassesComplete subscribers share the allocation.
        let nodes_arc = std::sync::Arc::from(all_nodes.into_boxed_slice());
        let edges_arc = std::sync::Arc::from(all_edges.into_boxed_slice());
        follow_ons.push(ExtractionEvent::PassesComplete {
            slug: slug.to_string(),
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
/// **Phase 3 real implementation.** Holds an `Arc<dyn Enricher>` for its language.
/// In `on_event(LanguageDetected)`, calls `enricher.enrich()` via
/// `tokio::task::block_in_place` (safe in a tokio multi-thread context) and returns a
/// real `EnrichmentComplete` with actual edges and virtual nodes.
///
/// Per the ADR: "Consumer of LanguageDetected(lang, nodes): LSP enrichers — ALL fire
/// concurrently, one per language." Parallel execution across languages falls out of
/// the architecture when the event bus gains async support (Phase 4+). In the current
/// synchronous bus each `LspConsumer` runs in registration order, but each runs to
/// completion so the enricher I/O is properly serialized without races.
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

        // Run async LSP enrichment synchronously within the sync event bus.
        // `block_in_place` offloads the current task off a tokio worker thread so
        // the runtime can schedule other work while we block. This is ONLY safe in
        // a multi-threaded tokio runtime (the MCP server path uses `tokio::main`
        // which defaults to multi-thread).
        //
        // For single-threaded runtimes (unit tests using `#[tokio::test]`) or
        // non-tokio contexts, we skip LSP enrichment gracefully.
        let enrichment_result = match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| {
                    handle.block_on(async {
                        // Build an index from the nodes visible to this enricher so that
                        // enrichers that resolve or deduplicate through the index (e.g., future
                        // enrichers beyond LspEnricher) see a populated graph rather than an
                        // empty one. LspEnricher currently ignores the index (_index param),
                        // but this keeps the contract correct for any enricher that does use it.
                        let mut index = crate::graph::index::GraphIndex::new();
                        for node in &nodes_vec {
                            index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                        }
                        enricher.enrich(&nodes_vec, &index, &repo_root).await
                    })
                })
            }
            Ok(_) => {
                // Single-threaded runtime (e.g., #[tokio::test]) — block_in_place would
                // panic. Skip LSP enrichment and emit empty EnrichmentComplete so
                // AllEnrichmentsGate can proceed.
                tracing::debug!(
                    "LspConsumer({}): single-thread runtime, skipping enrichment (test path)",
                    self.language,
                );
                Ok(crate::extract::EnrichmentResult::default())
            }
            Err(_) => {
                // No tokio runtime — return empty enrichment (sync-only path).
                tracing::debug!(
                    "LspConsumer({}): no tokio runtime, skipping enrichment",
                    self.language,
                );
                Ok(crate::extract::EnrichmentResult::default())
            }
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
                }])
            }
        }
    }
}

// ---------------------------------------------------------------------------
// AllEnrichmentsGate
// ---------------------------------------------------------------------------

/// Counts expected `LanguageDetected` events and received `EnrichmentComplete` events.
///
/// When the counts match, emits `AllEnrichmentsDone` with the merged LSP results.
/// This is the synchronisation point that lets `PostExtractionConsumer` wait for all
/// LSP enrichers to finish before running post-extraction passes.
///
/// **Singleton per bus instance.** One gate serves all languages for a single root.
/// Multiple roots would need separate gate instances (each bus is per-call in
/// `run_post_passes_via_bus`).
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
        }
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
        &[ExtractionEventKind::RootExtracted, ExtractionEventKind::EnrichmentComplete]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        let mut state = self.state.lock().expect("AllEnrichmentsGate mutex poisoned");

        match event {
            ExtractionEvent::RootExtracted { slug, nodes, edges, .. } => {
                // Count distinct languages in the node set to know how many
                // `EnrichmentComplete` events to expect. This must agree with
                // `LanguageAccumulatorConsumer` which emits one `LanguageDetected`
                // per distinct non-empty language.
                let language_count: usize = {
                    let mut seen = std::collections::HashSet::new();
                    for n in nodes.iter() {
                        if !n.language.is_empty() {
                            seen.insert(n.language.clone());
                        }
                    }
                    seen.len()
                };

                // Filter to languages that have a registered LspConsumer (i.e., are
                // supported by EnricherRegistry). Languages without an enricher will
                // never produce EnrichmentComplete, so we must not count them.
                let supported = crate::extract::EnricherRegistry::with_builtins()
                    .supported_languages();
                let expected = {
                    let mut seen = std::collections::HashSet::new();
                    for n in nodes.iter() {
                        if !n.language.is_empty() && supported.contains(&n.language) {
                            seen.insert(n.language.clone());
                        }
                    }
                    seen.len()
                };

                tracing::debug!(
                    "AllEnrichmentsGate: root '{}' — {} distinct language(s) in nodes, \
                     {} supported by EnricherRegistry",
                    slug,
                    language_count,
                    expected,
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
                // so PostExtractionConsumer doesn't wait forever.
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
/// **Phase 3 wiring**: `LspConsumer` instances are now real — they hold
/// `Arc<dyn Enricher>` and run actual LSP enrichment in `on_event`.
/// `AllEnrichmentsGate` gates `PostExtractionConsumer` so passes run only after
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
/// - `PostExtractionConsumer` — `AllEnrichmentsDone` → `PassesComplete` + `FrameworkDetected`
/// - `OpenApiConsumer`, `GrpcConsumer`, `EmbeddingIndexerConsumer` — `RootExtracted` (side-effects)
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
/// `repo_root` is the path to the primary repository root (for LSP server startup).
/// `scan_stats` is the shared stats handle owned by `RnaHandler`. When `None`,
/// a fresh throwaway `Arc` is created (used by `run_post_passes_via_bus` and tests).
///
/// Returns `(bus, scan_stats_arc)` where `scan_stats_arc` is either the passed-in
/// `Arc` or the freshly created one.
pub fn build_builtin_bus(
    root_pairs: Vec<(String, PathBuf)>,
    primary_slug: String,
    repo_root: PathBuf,
    scan_stats: Option<Arc<RwLock<ScanStats>>>,
) -> (crate::extract::event_bus::EventBus, Arc<RwLock<ScanStats>>) {
    use crate::extract::event_bus::EventBus;

    let mut bus = EventBus::new();

    // --- ScanStatsConsumer (singleton — registered first so it sees every event) ---
    let stats_arc = scan_stats.unwrap_or_else(|| Arc::new(RwLock::new(ScanStats::default())));
    let scan_stats_consumer = ScanStatsConsumer { stats: Arc::clone(&stats_arc) };
    bus.register(Box::new(scan_stats_consumer));

    // --- RootDiscovered consumers ---
    bus.register(Box::new(ManifestConsumer));
    bus.register(Box::new(TreeSitterConsumer::new()));

    // --- RootExtracted consumers ---
    // LanguageAccumulatorConsumer must run first (emits LanguageDetected which
    // triggers LspConsumers, then AllEnrichmentsGate counts up).
    bus.register(Box::new(LanguageAccumulatorConsumer));

    // AllEnrichmentsGate: must subscribe to RootExtracted BEFORE LspConsumers
    // fire so it captures the language count before any EnrichmentComplete arrives.
    // Registration order matches subscription order in the sync bus.
    bus.register(Box::new(AllEnrichmentsGate::new()));

    // OpenApi, gRPC, Embedding — subscribe to RootExtracted independently.
    bus.register(Box::new(OpenApiConsumer));
    bus.register(Box::new(GrpcConsumer));
    // EmbeddingIndexerConsumer: streaming embed as nodes arrive (Phase 2 stub)
    bus.register(Box::new(EmbeddingIndexerConsumer));

    // --- LanguageDetected consumers (one real LspConsumer per language) ---
    // Per the ADR: "ALL fire concurrently, one per language."
    // Currently sequential in the sync bus; Phase 4 promotes to async parallel.
    //
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

    // --- AllEnrichmentsDone consumers ---
    // PostExtractionConsumer now subscribes to AllEnrichmentsDone (Phase 3).
    bus.register(Box::new(PostExtractionConsumer::new(root_pairs.clone(), primary_slug)));

    // --- FrameworkDetected consumers ---
    bus.register(Box::new(FrameworkDetectionConsumer));
    bus.register(Box::new(NextjsRoutingConsumer::new(root_pairs)));
    bus.register(Box::new(PubSubConsumer));
    bus.register(Box::new(WebSocketConsumer));

    // --- PassesComplete consumers ---
    bus.register(Box::new(SubsystemConsumer));
    bus.register(Box::new(LanceDBConsumer));

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
/// * `scan_stats` - optional shared stats handle from `RnaHandler`. When provided,
///   the `ScanStatsConsumer` writes into this `Arc`; when `None`, a throwaway is used.
///
/// # Returns
/// `Ok((nodes, edges, detected_frameworks))` — the enriched graph and framework set.
///
/// # Errors
/// Returns `Err` if the bus does not produce a `PassesComplete` event.
/// This is a pipeline invariant violation (`PostExtractionConsumer` always emits
/// `PassesComplete`), so `Err` here indicates a logic bug, not a transient error.
/// Callers should propagate this as a hard failure so the pipeline can retry on
/// the next scan rather than silently persisting an unenriched graph.
pub fn run_post_passes_via_bus(
    nodes: Vec<crate::graph::Node>,
    edges: Vec<crate::graph::Edge>,
    root_pairs: Vec<(String, PathBuf)>,
    primary_slug: String,
    repo_root: PathBuf,
    scan_stats: Option<Arc<RwLock<ScanStats>>>,
) -> anyhow::Result<(Vec<crate::graph::Node>, Vec<crate::graph::Edge>, std::collections::HashSet<String>)> {
    use crate::extract::event_bus::ExtractionEvent;
    use std::sync::Arc;

    // Wrap into Arc<[T]> for zero-copy bus fan-out.
    let nodes_arc: Arc<[crate::graph::Node]> = Arc::from(nodes.into_boxed_slice());
    let edges_arc: Arc<[crate::graph::Edge]> = Arc::from(edges.into_boxed_slice());

    // Use repo_root for LSP server startup directory so LspConsumers have the
    // correct working directory. The path is also passed as the RootExtracted
    // event path so consumers that need it (e.g., tree-sitter-based consumers) work.
    let (bus, _stats) = build_builtin_bus(root_pairs, primary_slug.clone(), repo_root.clone(), scan_stats);
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
        let (bus, _stats) = build_builtin_bus(vec![], "test".into(), PathBuf::from("."), None);
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
            1 +        // AllEnrichmentsDone: PostExtractionConsumer (Phase 3 subscription)
            4 +        // FrameworkDetected: FrameworkDetectionConsumer, NextjsRoutingConsumer,
                       //   PubSubConsumer, WebSocketConsumer
            2;         // PassesComplete: SubsystemConsumer, LanceDBConsumer
        assert_eq!(
            bus.len(), expected_total,
            "Unexpected built-in consumer count: got {}, expected {} (lsp_count={})",
            bus.len(), expected_total, lsp_count
        );
    }

    /// Verify build_builtin_bus returns a stats handle that shares state with the bus.
    #[test]
    fn test_builtin_bus_returns_scan_stats_handle() {
        let (bus, stats) = build_builtin_bus(vec![], "test".into(), PathBuf::from("."), None);
        // No activity yet
        assert!(!stats.read().unwrap().has_activity());

        // Emit RootDiscovered — the ScanStatsConsumer inside the bus should update stats.
        bus.emit(crate::extract::event_bus::ExtractionEvent::RootDiscovered {
            slug: "test".into(),
            path: PathBuf::from("."),
            lsp_only: false,
        });
        assert!(
            stats.read().unwrap().has_activity(),
            "stats handle must reflect events fired through the bus"
        );
    }

    /// Verify PostExtractionConsumer emits PassesComplete on empty input.
    /// Phase 3: now subscribes to AllEnrichmentsDone (not RootExtracted).
    #[test]
    fn test_post_extraction_consumer_emits_passes_complete() {
        let consumer = PostExtractionConsumer::new(vec![], "test".into());
        // Must use AllEnrichmentsDone (Phase 3 subscription).
        let event = ExtractionEvent::AllEnrichmentsDone {
            slug: "test".into(),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
            lsp_edges: std::sync::Arc::from([]),
            lsp_nodes: std::sync::Arc::from([]),
            updated_nodes: std::sync::Arc::from([]),
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

        let (bus, _stats) = build_builtin_bus(
            vec![("test".to_string(), tmp.path().to_path_buf())],
            "test".to_string(),
            tmp.path().to_path_buf(),
            None,
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
    /// Uses a no-op enricher so the test doesn't try to start a real LSP server.
    #[test]
    fn test_lsp_consumer_fires_for_declared_language() {
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
        let result = consumer.on_event(&matching).unwrap();
        // No tokio runtime in sync test context: the consumer falls back to the no-op path
        // and still emits EnrichmentComplete (with empty edges).
        assert_eq!(result.len(), 1, "LspConsumer must emit EnrichmentComplete for its language");
        assert!(
            matches!(result[0], ExtractionEvent::EnrichmentComplete { .. }),
            "LspConsumer must emit EnrichmentComplete"
        );
    }

    #[test]
    fn test_lsp_consumer_ignores_other_language() {
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
        let result = consumer.on_event(&other_lang).unwrap();
        assert!(result.is_empty(), "LspConsumer must ignore events for other languages");
    }

    /// Verify AllEnrichmentsGate emits AllEnrichmentsDone immediately when no supported languages.
    #[test]
    fn test_all_enrichments_gate_no_languages() {
        let gate = AllEnrichmentsGate::new();
        // No nodes → no languages → gate fires immediately on RootExtracted.
        let event = ExtractionEvent::RootExtracted {
            slug: "test".into(),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from([]),
            edges: std::sync::Arc::from([]),
        };
        let result = gate.on_event(&event).unwrap();
        assert_eq!(result.len(), 1, "Gate must emit AllEnrichmentsDone when expected==0");
        assert!(
            matches!(result[0], ExtractionEvent::AllEnrichmentsDone { .. }),
            "Expected AllEnrichmentsDone"
        );
    }

    /// Verify AllEnrichmentsGate waits for all enrichments before emitting.
    #[test]
    fn test_all_enrichments_gate_waits_for_all() {
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
        };
        let result = gate.on_event(&root_extracted).unwrap();
        // expected=1, received=0 → no AllEnrichmentsDone yet.
        assert!(result.is_empty(), "Gate must not fire before all enrichments arrive");

        // Send EnrichmentComplete for rust.
        let enrichment_done = ExtractionEvent::EnrichmentComplete {
            slug: "test".into(),
            language: "rust".into(),
            added_edges: std::sync::Arc::from([]),
            new_nodes: std::sync::Arc::from([]),
            updated_nodes: std::sync::Arc::from([]),
        };
        let result = gate.on_event(&enrichment_done).unwrap();
        assert_eq!(result.len(), 1, "Gate must emit AllEnrichmentsDone after all enrichments");
        assert!(
            matches!(result[0], ExtractionEvent::AllEnrichmentsDone { .. }),
            "Expected AllEnrichmentsDone"
        );
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
    #[test]
    fn test_run_post_passes_via_bus_empty_input() {
        let (nodes, edges, frameworks) = super::run_post_passes_via_bus(
            vec![],
            vec![],
            vec![],
            "test".to_string(),
            PathBuf::from("."),
            None,
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
            None,
        ).expect("run_post_passes_via_bus must not fail with valid input");

        // Output must contain the input node (passes should not drop it)
        assert!(
            out_nodes.iter().any(|n| n.id.name == "my_fn"),
            "run_post_passes_via_bus must preserve input nodes through post-extraction passes"
        );
    }
}
