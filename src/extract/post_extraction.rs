//! Plugin architecture for post-extraction passes.
//!
//! # Overview
//!
//! The `PostExtractionRegistry` replaces the manual sequence of
//! `if should_run { pass() }` calls that previously lived in both
//! `build_full_graph_inner` and `update_graph_with_scan`. Every post-extraction
//! enrichment step is now a `PostExtractionPass` implementor registered once.
//!
//! # Key properties
//!
//! - **Framework-gated** — `applies_when()` is called before each pass. Passes
//!   whose framework is not present are skipped at zero cost.
//! - **Auto-delta collection** — the registry snapshots node/edge counts before
//!   and after each pass, making the cumulative delta available as
//!   [`RegistryResult::added_node_count`] / [`RegistryResult::added_edge_count`].
//!   No pass needs to manually report what it added (fixes #474).
//! - **Single call site** — `PostExtractionRegistry::run_all` is called from
//!   `run_post_extraction_passes` which is invoked from both
//!   `build_full_graph_inner` AND the background scanner (fixes #471).
//!
//! # Adding a new pass
//!
//! 1. Implement `PostExtractionPass`.
//! 2. Add it to `PostExtractionRegistry::with_builtins()`.
//! Done — no changes to `graph.rs` or `enrichment.rs`.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::graph::{Edge, Node};

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Immutable context passed to every pass at run time.
///
/// The registry populates this once and shares a reference with all passes.
/// Passes that are unconditional (api_link, tested_by, import_calls,
/// directory_module) ignore most fields. Passes that are filesystem-aware
/// (manifest, nextjs_routing) use `root_pairs`. Framework-gated passes use
/// `detected_frameworks`.
#[derive(Debug, Clone)]
pub struct PassContext {
    /// `(slug, path)` pairs for all workspace roots. Used by manifest and
    /// Next.js routing passes which need filesystem access.
    pub root_pairs: Vec<(String, PathBuf)>,
    /// Slug of the primary code root. Used by framework, pub/sub, WebSocket,
    /// and subsystem passes to anchor virtual nodes.
    pub primary_slug: String,
    /// Frameworks detected so far (updated after framework_detection_pass runs).
    /// Passes that gate on a specific framework check this set in `applies_when`.
    pub detected_frameworks: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Pass result
// ---------------------------------------------------------------------------

/// The metadata returned by a single pass.
///
/// Passes extend `nodes` and `edges` in-place (the registry auto-tracks the
/// delta via length snapshotting). The only value a pass needs to return is the
/// set of detected frameworks — the registry propagates this into `PassContext`
/// so subsequent framework-gated passes see the updated set.
#[derive(Debug, Default)]
pub struct PassResult {
    /// Frameworks detected by this pass (non-empty only for `framework_detection_pass`).
    /// The registry merges these into `PassContext::detected_frameworks` before running
    /// the next pass.
    pub detected_frameworks: HashSet<String>,
}

impl PassResult {
    /// Convenience: pass produced no framework detections.
    pub fn empty() -> Self {
        Self::default()
    }
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// A single post-extraction enrichment step.
///
/// Passes are stateless — they receive all their input via `ctx` + the mutable
/// graph vecs, and return a `PassResult`.
pub trait PostExtractionPass: Send + Sync {
    /// Human-readable identifier used in log messages and diagnostics.
    fn name(&self) -> &str;

    /// Return `true` if this pass should run given the detected frameworks.
    ///
    /// The default implementation returns `true` (always run). Passes that
    /// only make sense in the presence of a specific framework should override
    /// this to check `ctx.detected_frameworks`.
    fn applies_when(&self, detected_frameworks: &HashSet<String>) -> bool {
        let _ = detected_frameworks;
        true
    }

    /// Run the pass against the current node/edge set.
    ///
    /// The pass **appends** any new nodes/edges to `nodes` and `edges` — it
    /// does not replace them. The registry auto-collects the delta by
    /// snapshotting lengths before and after.
    fn run(&self, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>, ctx: &PassContext) -> PassResult;
}

// ---------------------------------------------------------------------------
// Registry result
// ---------------------------------------------------------------------------

/// Summary returned by [`PostExtractionRegistry::run_all`].
#[derive(Debug, Default)]
pub struct RegistryResult {
    /// Total new nodes added by all passes combined.
    pub added_node_count: usize,
    /// Total new edges added by all passes combined.
    pub added_edge_count: usize,
    /// Union of all detected frameworks across all passes.
    pub detected_frameworks: HashSet<String>,
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Ordered collection of [`PostExtractionPass`] implementors.
///
/// Passes run in registration order. The registry:
/// 1. Calls `applies_when` before each pass — skips inapplicable passes.
/// 2. Auto-collects the node/edge delta for each pass.
/// 3. Propagates detected frameworks from framework-aware passes so subsequent
///    framework-gated passes see the updated set.
pub struct PostExtractionRegistry {
    passes: Vec<Box<dyn PostExtractionPass>>,
}

impl PostExtractionRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self { passes: Vec::new() }
    }

    /// Register a pass. Passes run in registration order.
    pub fn register(&mut self, pass: Box<dyn PostExtractionPass>) {
        self.passes.push(pass);
    }

    /// Run all registered passes in order.
    ///
    /// Framework detection runs unconditionally (it sets `detected_frameworks`
    /// in the context). All subsequent passes receive the updated context.
    ///
    /// Returns a `RegistryResult` with the cumulative delta and detected
    /// frameworks (for callers that store this on `GraphState`).
    pub fn run_all(
        &self,
        nodes: &mut Vec<Node>,
        edges: &mut Vec<Edge>,
        mut ctx: PassContext,
    ) -> RegistryResult {
        let mut result = RegistryResult::default();

        for pass in &self.passes {
            if !pass.applies_when(&ctx.detected_frameworks) {
                tracing::debug!("PostExtractionPass '{}': skipped (applies_when=false)", pass.name());
                continue;
            }

            let nodes_before = nodes.len();
            let edges_before = edges.len();

            let pass_result = pass.run(nodes, edges, &ctx);

            let added_nodes = nodes.len().saturating_sub(nodes_before);
            let added_edges = edges.len().saturating_sub(edges_before);

            if added_nodes > 0 || added_edges > 0 || !pass_result.detected_frameworks.is_empty() {
                tracing::info!(
                    "PostExtractionPass '{}': +{} node(s), +{} edge(s){}",
                    pass.name(),
                    added_nodes,
                    added_edges,
                    if !pass_result.detected_frameworks.is_empty() {
                        format!(", detected: {:?}", pass_result.detected_frameworks)
                    } else {
                        String::new()
                    }
                );
            } else {
                tracing::debug!("PostExtractionPass '{}': no output", pass.name());
            }

            result.added_node_count += added_nodes;
            result.added_edge_count += added_edges;

            // Propagate detected frameworks so subsequent passes (pubsub, ws, nextjs)
            // see the updated set via `applies_when`.
            if !pass_result.detected_frameworks.is_empty() {
                ctx.detected_frameworks
                    .extend(pass_result.detected_frameworks.iter().cloned());
                result.detected_frameworks
                    .extend(pass_result.detected_frameworks);
            }
        }

        result
    }

    /// Build the default registry with all built-in passes in the standard order.
    ///
    /// **Registration order is significant.** Passes run in the order they are
    /// registered. The invariant that must be preserved:
    ///
    /// 1. **Unconditional passes first** (api_link, manifest, tested_by, import_calls,
    ///    dir_module) — these depend only on extracted nodes/edges
    /// 2. **`FrameworkDetectionPass` before any framework-gated pass** — it sets
    ///    `detected_frameworks` in the context; gated passes (pubsub, websocket) call
    ///    `applies_when` which checks that set. Inserting a gated pass before
    ///    `FrameworkDetectionPass` causes it to skip silently even when its framework
    ///    is present.
    /// 3. **Framework-gated passes last** (nextjs, pubsub, websocket)
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        // Group 1: unconditional passes (no framework dependency)
        reg.register(Box::new(ApiLinkPass));
        reg.register(Box::new(ManifestPass));
        reg.register(Box::new(TestedByPass));
        reg.register(Box::new(ImportCallsPass));
        reg.register(Box::new(DirectoryModulePass));
        // Group 2: framework detection — MUST run before any framework-gated pass
        reg.register(Box::new(FrameworkDetectionPass));
        // Group 3: framework-gated passes (pubsub/websocket use applies_when; nextjs gates in run())
        reg.register(Box::new(NextjsRoutingPass));
        reg.register(Box::new(PubSubPass));
        reg.register(Box::new(WebSocketPass));
        reg
    }
}

impl Default for PostExtractionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Built-in pass implementations
// ---------------------------------------------------------------------------

// --- ApiLinkPass ---

struct ApiLinkPass;

impl PostExtractionPass for ApiLinkPass {
    fn name(&self) -> &str { "api_link" }

    fn run(&self, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>, _ctx: &PassContext) -> PassResult {
        let new_edges = crate::extract::api_link::api_link_pass(nodes);
        if !new_edges.is_empty() {
            edges.extend(new_edges);
        }
        PassResult::empty()
    }
}

// --- ManifestPass ---

struct ManifestPass;

impl PostExtractionPass for ManifestPass {
    fn name(&self) -> &str { "manifest" }

    fn run(&self, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>, ctx: &PassContext) -> PassResult {
        let result = crate::extract::manifest::manifest_pass(&ctx.root_pairs);
        if !result.nodes.is_empty() || !result.edges.is_empty() {
            nodes.extend(result.nodes);
            edges.extend(result.edges);
        }
        PassResult::empty()
    }
}

// --- TestedByPass ---

struct TestedByPass;

impl PostExtractionPass for TestedByPass {
    fn name(&self) -> &str { "tested_by" }

    fn run(&self, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>, _ctx: &PassContext) -> PassResult {
        let new_edges = crate::extract::naming_convention::tested_by_pass(nodes);
        if !new_edges.is_empty() {
            edges.extend(new_edges);
        }
        PassResult::empty()
    }
}

// --- ImportCallsPass ---

struct ImportCallsPass;

impl PostExtractionPass for ImportCallsPass {
    fn name(&self) -> &str { "import_calls" }

    fn run(&self, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>, _ctx: &PassContext) -> PassResult {
        let new_edges = crate::extract::import_calls::import_calls_pass(nodes);
        if !new_edges.is_empty() {
            edges.extend(new_edges);
        }
        PassResult::empty()
    }
}

// --- DirectoryModulePass ---

struct DirectoryModulePass;

impl PostExtractionPass for DirectoryModulePass {
    fn name(&self) -> &str { "directory_module" }

    fn run(&self, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>, _ctx: &PassContext) -> PassResult {
        let result = crate::extract::directory_module::directory_module_pass(nodes);
        if !result.nodes.is_empty() || !result.edges.is_empty() {
            nodes.extend(result.nodes);
            edges.extend(result.edges);
        }
        PassResult::empty()
    }
}

// --- FrameworkDetectionPass ---

struct FrameworkDetectionPass;

impl PostExtractionPass for FrameworkDetectionPass {
    fn name(&self) -> &str { "framework_detection" }

    // Always runs — it populates detected_frameworks for subsequent passes.
    fn applies_when(&self, _detected_frameworks: &HashSet<String>) -> bool { true }

    fn run(&self, nodes: &mut Vec<Node>, _edges: &mut Vec<Edge>, ctx: &PassContext) -> PassResult {
        let result =
            crate::extract::framework_detection::framework_detection_pass(nodes, &ctx.primary_slug);
        let detected = result.detected_frameworks.clone();
        if !result.nodes.is_empty() {
            nodes.extend(result.nodes);
        }
        PassResult {
            detected_frameworks: detected,
        }
    }
}

// --- NextjsRoutingPass ---

struct NextjsRoutingPass;

impl PostExtractionPass for NextjsRoutingPass {
    fn name(&self) -> &str { "nextjs_routing" }

    // Always attempt to run — the `run` method gates on actual content
    // (nextjs-app-router framework OR TypeScript/JavaScript files present).
    // We cannot detect TS/JS presence in `applies_when` without inspecting
    // nodes, so we defer the full check to `run`.
    fn applies_when(&self, _detected_frameworks: &HashSet<String>) -> bool { true }

    fn run(&self, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>, ctx: &PassContext) -> PassResult {
        // Run if Next.js was detected OR if TS/JS files are present.
        let has_ts_js = nodes.iter().any(|n| {
            matches!(n.language.as_str(), "typescript" | "javascript")
        });
        if !ctx.detected_frameworks.contains("nextjs-app-router") && !has_ts_js {
            return PassResult::empty();
        }

        let result = crate::extract::nextjs_routing::nextjs_routing_pass(&ctx.root_pairs, nodes);
        if !result.nodes.is_empty() || !result.edges.is_empty() {
            nodes.extend(result.nodes);
            edges.extend(result.edges);
        }
        PassResult::empty()
    }
}

// --- PubSubPass ---

struct PubSubPass;

impl PostExtractionPass for PubSubPass {
    fn name(&self) -> &str { "pubsub" }

    fn applies_when(&self, detected_frameworks: &HashSet<String>) -> bool {
        crate::extract::pubsub::should_run(detected_frameworks)
    }

    fn run(&self, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>, ctx: &PassContext) -> PassResult {
        let result = crate::extract::pubsub::pubsub_pass(
            nodes,
            &ctx.detected_frameworks,
            &ctx.primary_slug,
        );
        if !result.edges.is_empty() {
            nodes.extend(result.nodes);
            edges.extend(result.edges);
        }
        PassResult::empty()
    }
}

// --- WebSocketPass ---

struct WebSocketPass;

impl PostExtractionPass for WebSocketPass {
    fn name(&self) -> &str { "websocket" }

    fn applies_when(&self, detected_frameworks: &HashSet<String>) -> bool {
        crate::extract::websocket::should_run(detected_frameworks)
    }

    fn run(&self, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>, ctx: &PassContext) -> PassResult {
        let result = crate::extract::websocket::websocket_pass(
            nodes,
            &ctx.detected_frameworks,
            &ctx.primary_slug,
        );
        if !result.edges.is_empty() {
            nodes.extend(result.nodes);
            edges.extend(result.edges);
        }
        PassResult::empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ExtractionSource, NodeId, NodeKind};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_node(root: &str, file: &str, name: &str, kind: NodeKind, lang: &str) -> Node {
        Node {
            id: NodeId {
                root: root.to_string(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind,
            },
            language: lang.to_string(),
            line_start: 1,
            line_end: 1,
            signature: name.to_string(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn empty_ctx() -> PassContext {
        PassContext {
            root_pairs: vec![],
            primary_slug: "test".to_string(),
            detected_frameworks: HashSet::new(),
        }
    }

    #[test]
    fn test_registry_empty_input() {
        let reg = PostExtractionRegistry::with_builtins();
        let mut nodes: Vec<Node> = vec![];
        let mut edges: Vec<crate::graph::Edge> = vec![];
        let result = reg.run_all(&mut nodes, &mut edges, empty_ctx());
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
        assert_eq!(result.added_node_count, 0);
        assert_eq!(result.added_edge_count, 0);
        assert!(result.detected_frameworks.is_empty());
    }

    #[test]
    fn test_registry_applies_when_skips_gated_passes() {
        // With no frameworks detected and no TS/JS files, pubsub/websocket/nextjs
        // should be skipped. Only the unconditional passes run.
        let reg = PostExtractionRegistry::with_builtins();
        let mut nodes = vec![make_node("r", "src/foo.rs", "foo", NodeKind::Function, "rust")];
        let mut edges: Vec<crate::graph::Edge> = vec![];
        let result = reg.run_all(&mut nodes, &mut edges, empty_ctx());
        // No frameworks detected for a single Rust function node
        assert!(result.detected_frameworks.is_empty());
    }

    #[test]
    fn test_registry_framework_detection_updates_context() {
        // Verify that a pass that emits detected_frameworks causes subsequent
        // passes to see the updated set via applies_when.
        //
        // Registry: FwEmitPass (emits "some-framework") → GatedPass (applies only
        // when "some-framework" is present). If propagation works, GatedPass runs
        // and adds a node; if it doesn't, GatedPass is skipped.

        struct FwEmitPass;
        impl PostExtractionPass for FwEmitPass {
            fn name(&self) -> &str { "fw_emit" }
            fn run(&self, _n: &mut Vec<Node>, _e: &mut Vec<crate::graph::Edge>, _c: &PassContext) -> PassResult {
                let mut fw = HashSet::new();
                fw.insert("some-framework".to_string());
                PassResult { detected_frameworks: fw }
            }
        }

        struct GatedPass;
        impl PostExtractionPass for GatedPass {
            fn name(&self) -> &str { "gated" }
            fn applies_when(&self, detected_frameworks: &HashSet<String>) -> bool {
                detected_frameworks.contains("some-framework")
            }
            fn run(&self, nodes: &mut Vec<Node>, _e: &mut Vec<crate::graph::Edge>, _c: &PassContext) -> PassResult {
                // Add a sentinel node to prove we ran
                nodes.push(make_node("r", "sentinel", "sentinel", NodeKind::Function, "rust"));
                PassResult::empty()
            }
        }

        let mut reg = PostExtractionRegistry::new();
        reg.register(Box::new(FwEmitPass));
        reg.register(Box::new(GatedPass));

        let mut nodes: Vec<Node> = vec![];
        let mut edges: Vec<crate::graph::Edge> = vec![];
        let result = reg.run_all(&mut nodes, &mut edges, empty_ctx());

        // FwEmitPass populated detected_frameworks → GatedPass ran → sentinel node added
        assert!(result.detected_frameworks.contains("some-framework"), "framework should be detected");
        assert!(
            nodes.iter().any(|n| n.id.name == "sentinel"),
            "GatedPass should have run after FwEmitPass propagated the framework"
        );
    }

    #[test]
    fn test_registry_idempotent_with_dedup() {
        // Running the registry twice (with dedup between) should not grow edge count.
        let reg = PostExtractionRegistry::with_builtins();
        let node1 = make_node("root", "src/foo.rs", "test_foo", NodeKind::Function, "rust");
        let node2 = make_node("root", "src/foo.rs", "foo", NodeKind::Function, "rust");
        let mut nodes = vec![node1, node2];
        let mut edges: Vec<crate::graph::Edge> = vec![];

        let _r1 = reg.run_all(&mut nodes, &mut edges, empty_ctx());
        let edge_count_after_first = edges.len();

        // Dedup (mirrors caller)
        let mut seen = HashSet::new();
        edges.retain(|e| seen.insert(e.stable_id()));

        let _r2 = reg.run_all(&mut nodes, &mut edges, empty_ctx());
        let mut seen2 = HashSet::new();
        edges.retain(|e| seen2.insert(e.stable_id()));

        assert_eq!(
            edge_count_after_first,
            edges.len(),
            "passes are idempotent when dedup runs between calls"
        );
    }
}
