//! Event bus for the extraction pipeline.
//!
//! # Architecture
//!
//! The event bus decouples pipeline stages: each stage is an `ExtractionConsumer`
//! that declares which events wake it up. No consumer knows about any other consumer.
//! The bus is the only coupling.
//!
//! **Static registration, dynamic routing.** All consumers register at startup.
//! The registry is fixed before any events fire. When an event fires
//! (e.g. `FrameworkDetected("nextjs-app-router")`), the bus routes it to
//! already-registered subscribers. There is no dynamic consumer creation at runtime.
//!
//! # Event flow
//!
//! ```text
//! Bootstrap:
//!   RootDiscovered(slug, path)
//!     → ManifestConsumer (per root)
//!     → TreeSitterConsumer → RootExtracted(slug, nodes, edges)
//!         → LanguageAccumulatorConsumer → LanguageDetected(lang, nodes) per language
//!             → LspConsumer (one per language, sequential in Phase 2)
//!               → EnrichmentComplete(lang, edges)
//!         → PostExtractionConsumer → PassesComplete(nodes, edges)
//!             → SubsystemConsumer → SubsystemsDetected(...)
//!             → LanceDBConsumer (persist)
//!         → EmbeddingConsumer (streaming)
//! ```
//!
//! # Adding a new consumer
//!
//! 1. Implement `ExtractionConsumer`.
//! 2. Register it in `EventBus::with_builtins()`.
//! Done — no changes to `graph.rs` or `enrichment.rs`.
//!
//! # ADR audit constraints
//!
//! - Consumers live in `src/extract/` and must NOT import from each other.
//! - No `register()` calls inside `on_event()`.
//! - No broker-specific knowledge (`kafka`, `pubsub`, `rabbitmq`) in RNA core.
//! - All pipeline paths go through `EventBus::run()`.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use crate::graph::{Edge, Node};

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// All events the extraction pipeline can emit.
///
/// Each variant carries the data consumers need — no consumer polls context.
///
/// **Shared ownership via `Arc<[T]>`**: Node/edge payloads use `Arc<[Node]>` and
/// `Arc<[Edge]>` instead of `Vec<T>` to avoid O(N) clones when multiple consumers
/// receive the same event. The bus fans out events by `clone()` — with `Vec`, each
/// consumer gets a full deep copy; with `Arc`, consumers share the same allocation.
///
/// Constructing a payload: `Arc::from(vec.into_boxed_slice())` or
/// `Arc::from(slice)`. Reading: `event.nodes.iter()` works as with Vec.
#[derive(Debug, Clone)]
pub enum ExtractionEvent {
    /// A repository root has been discovered. First event in the pipeline.
    RootDiscovered {
        slug: String,
        path: PathBuf,
        lsp_only: bool,
    },

    /// Tree-sitter extraction is complete for a root.
    /// Carries all extracted nodes + edges for that root.
    RootExtracted {
        slug: String,
        path: PathBuf,
        /// Shared read-only view of all extracted nodes. Use `Arc::from(vec)` to construct.
        nodes: Arc<[Node]>,
        /// Shared read-only view of all extracted edges. Use `Arc::from(vec)` to construct.
        edges: Arc<[Edge]>,
    },

    /// A language has been detected in the extracted nodes.
    /// Fired once per language per root. Carries only nodes for that language.
    LanguageDetected {
        slug: String,
        language: String,
        /// Nodes for this specific language — subset of the root's nodes.
        nodes: Arc<[Node]>,
    },

    /// LSP enrichment is complete for a language.
    EnrichmentComplete {
        slug: String,
        language: String,
        added_edges: Arc<[Edge]>,
        new_nodes: Arc<[Node]>,
    },

    /// A framework has been detected during post-extraction passes.
    /// Consumers that gate on a specific framework check `framework` here.
    FrameworkDetected {
        slug: String,
        framework: String,
        /// Nodes that were tagged with this framework during detection.
        nodes: Arc<[Node]>,
    },

    /// A single post-extraction pass has completed.
    PassComplete {
        pass_name: &'static str,
        added_nodes: usize,
        added_edges: usize,
    },

    /// All post-extraction passes are complete for a root.
    /// Terminal consumers (LanceDBPersist, SubsystemPass) subscribe here.
    PassesComplete {
        slug: String,
        /// Full enriched node set after all passes ran.
        nodes: Arc<[Node]>,
        /// Full enriched edge set after all passes ran.
        edges: Arc<[Edge]>,
        detected_frameworks: HashSet<String>,
    },

    /// All LSP enrichments for a root are done.
    ///
    /// Fired by `AllEnrichmentsGate` after every `EnrichmentComplete` for a given
    /// root has been received. Carries the merged LSP edges and virtual nodes from
    /// all per-language enrichers.
    ///
    /// `PostExtractionConsumer` subscribes to this event (Phase 3+) so that
    /// post-extraction passes run on LSP-enriched nodes. In Phase 2 it subscribed
    /// to `RootExtracted` and ran before LSP — this event fixes that ordering.
    AllEnrichmentsDone {
        slug: String,
        /// Base nodes from `RootExtracted` (tree-sitter output).
        nodes: Arc<[Node]>,
        /// Base edges from `RootExtracted` (tree-sitter output).
        edges: Arc<[Edge]>,
        /// Additional edges produced by LSP enrichment.
        lsp_edges: Arc<[Edge]>,
        /// Virtual nodes added by LSP enrichment (e.g., external callee stubs).
        lsp_nodes: Arc<[Node]>,
    },
}

/// Discriminant for `ExtractionEvent` — used in `subscribes_to`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExtractionEventKind {
    RootDiscovered,
    RootExtracted,
    LanguageDetected,
    EnrichmentComplete,
    FrameworkDetected,
    PassComplete,
    PassesComplete,
    AllEnrichmentsDone,
}

impl ExtractionEvent {
    /// Return the kind discriminant for this event.
    pub fn kind(&self) -> ExtractionEventKind {
        match self {
            ExtractionEvent::RootDiscovered { .. }  => ExtractionEventKind::RootDiscovered,
            ExtractionEvent::RootExtracted { .. }   => ExtractionEventKind::RootExtracted,
            ExtractionEvent::LanguageDetected { .. }  => ExtractionEventKind::LanguageDetected,
            ExtractionEvent::EnrichmentComplete { .. } => ExtractionEventKind::EnrichmentComplete,
            ExtractionEvent::FrameworkDetected { .. } => ExtractionEventKind::FrameworkDetected,
            ExtractionEvent::PassComplete { .. }    => ExtractionEventKind::PassComplete,
            ExtractionEvent::PassesComplete { .. }  => ExtractionEventKind::PassesComplete,
            ExtractionEvent::AllEnrichmentsDone { .. } => ExtractionEventKind::AllEnrichmentsDone,
        }
    }
}

// ---------------------------------------------------------------------------
// Consumer trait
// ---------------------------------------------------------------------------

/// A pipeline stage that reacts to events.
///
/// **Pure event subscription — no polling, no context checking.**
///
/// Consumers declare which events wake them up. When that event fires, they
/// run. No `applies_when`, no `ScanContext` — the event carries everything.
///
/// Consumers return a `Vec<ExtractionEvent>` — zero or more follow-on events
/// that the bus will route to other subscribers. This replaces the old pattern
/// of directly calling the next stage.
pub trait ExtractionConsumer: Send + Sync {
    /// Human-readable identifier for diagnostics.
    fn name(&self) -> &str;

    /// Which event kinds wake this consumer up.
    ///
    /// The bus calls `on_event` only for events whose kind appears in this slice.
    fn subscribes_to(&self) -> &[ExtractionEventKind];

    /// React to an event. Returns any follow-on events to emit.
    ///
    /// **Must not call `bus.register()` or create new consumers.**
    /// **Must not import or call other consumers directly.**
    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>>;
}

// ---------------------------------------------------------------------------
// Event bus
// ---------------------------------------------------------------------------

/// Ordered registry of `ExtractionConsumer` implementations.
///
/// All consumers register at startup. The bus routes events to subscribers
/// by matching `event.kind()` against each consumer's `subscribes_to()`.
///
/// The bus is **synchronous** in Phase 2. A consumer's `on_event` return
/// value (follow-on events) is appended to a work queue; the bus drains the
/// queue depth-first. This preserves the ordering invariant from the original
/// sequential pipeline.
pub struct EventBus {
    consumers: Vec<Box<dyn ExtractionConsumer>>,
}

impl EventBus {
    /// Create an empty bus.
    pub fn new() -> Self {
        Self { consumers: Vec::new() }
    }

    /// Register a consumer. Consumers run in registration order.
    pub fn register(&mut self, consumer: Box<dyn ExtractionConsumer>) {
        self.consumers.push(consumer);
    }

    /// Emit a single event and process all follow-on events depth-first.
    ///
    /// Returns all events emitted (including the seed), in emission order.
    /// This is primarily useful for testing.
    pub fn emit(&self, seed: ExtractionEvent) -> Vec<ExtractionEvent> {
        // Use VecDeque for O(1) front removal rather than O(n) Vec::remove(0).
        let mut queue: VecDeque<ExtractionEvent> = VecDeque::new();
        queue.push_back(seed);
        let mut emitted: Vec<ExtractionEvent> = Vec::new();

        while let Some(event) = queue.pop_front() {
            let kind = event.kind();

            // Collect follow-on events from all subscribers, in registration order.
            let mut follow_ons: Vec<ExtractionEvent> = Vec::new();
            for consumer in &self.consumers {
                if !consumer.subscribes_to().contains(&kind) {
                    continue;
                }
                match consumer.on_event(&event) {
                    Ok(mut new_events) => {
                        if !new_events.is_empty() {
                            tracing::debug!(
                                "EventBus: consumer '{}' emitted {} follow-on event(s) from {:?}",
                                consumer.name(),
                                new_events.len(),
                                kind,
                            );
                            follow_ons.append(&mut new_events);
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "EventBus: consumer '{}' failed on {:?}: {}",
                            consumer.name(),
                            kind,
                            e,
                        );
                        // Continue processing other consumers; one failure doesn't stop the bus.
                    }
                }
            }

            emitted.push(event);
            // Prepend follow-ons so depth-first ordering is preserved:
            // follow-ons from this event are processed before the next sibling.
            // drain remaining queue items to rebuild with follow-ons first.
            let remaining: Vec<_> = queue.drain(..).collect();
            for fo in follow_ons {
                queue.push_back(fo);
            }
            for r in remaining {
                queue.push_back(r);
            }
        }

        emitted
    }

    /// Emit multiple seed events (one per discovered root), process all follow-ons.
    pub fn emit_all(&self, seeds: impl IntoIterator<Item = ExtractionEvent>) -> Vec<ExtractionEvent> {
        let mut all: Vec<ExtractionEvent> = Vec::new();
        for seed in seeds {
            all.extend(self.emit(seed));
        }
        all
    }

    /// Number of registered consumers.
    pub fn len(&self) -> usize {
        self.consumers.len()
    }

    /// Whether the bus has no consumers registered.
    pub fn is_empty(&self) -> bool {
        self.consumers.is_empty()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

    struct CountingConsumer {
        name: &'static str,
        kinds: Vec<ExtractionEventKind>,
        count: Arc<AtomicUsize>,
    }

    impl ExtractionConsumer for CountingConsumer {
        fn name(&self) -> &str { self.name }
        fn subscribes_to(&self) -> &[ExtractionEventKind] { &self.kinds }
        fn on_event(&self, _event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
            self.count.fetch_add(1, Ordering::Relaxed);
            Ok(vec![])
        }
    }

    struct EmittingConsumer {
        name: &'static str,
        listens: ExtractionEventKind,
        emits: Vec<ExtractionEvent>,
    }

    impl ExtractionConsumer for EmittingConsumer {
        fn name(&self) -> &str { self.name }
        fn subscribes_to(&self) -> &[ExtractionEventKind] { std::slice::from_ref(&self.listens) }
        fn on_event(&self, _event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
            Ok(self.emits.clone())
        }
    }

    fn make_root_discovered() -> ExtractionEvent {
        ExtractionEvent::RootDiscovered {
            slug: "test".to_string(),
            path: PathBuf::from("."),
            lsp_only: false,
        }
    }

    fn make_root_extracted() -> ExtractionEvent {
        ExtractionEvent::RootExtracted {
            slug: "test".to_string(),
            path: PathBuf::from("."),
            nodes: Arc::from(vec![].into_boxed_slice()),
            edges: Arc::from(vec![].into_boxed_slice()),
        }
    }

    #[test]
    fn test_empty_bus_emits_nothing() {
        let bus = EventBus::new();
        let events = bus.emit(make_root_discovered());
        // Seed event is always in emitted list
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_consumer_receives_matching_event() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut bus = EventBus::new();
        bus.register(Box::new(CountingConsumer {
            name: "counter",
            kinds: vec![ExtractionEventKind::RootDiscovered],
            count: Arc::clone(&count),
        }));

        bus.emit(make_root_discovered());
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_consumer_ignores_non_matching_event() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut bus = EventBus::new();
        bus.register(Box::new(CountingConsumer {
            name: "counter",
            kinds: vec![ExtractionEventKind::PassesComplete],
            count: Arc::clone(&count),
        }));

        bus.emit(make_root_discovered());
        assert_eq!(count.load(Ordering::Relaxed), 0, "counter must not fire for non-matching event");
    }

    #[test]
    fn test_follow_on_events_are_routed() {
        // EmittingConsumer listens for RootDiscovered, emits RootExtracted.
        // CountingConsumer listens for RootExtracted.
        // If routing works, counting consumer fires once.
        let count = Arc::new(AtomicUsize::new(0));
        let mut bus = EventBus::new();

        bus.register(Box::new(EmittingConsumer {
            name: "emitter",
            listens: ExtractionEventKind::RootDiscovered,
            emits: vec![make_root_extracted()],
        }));
        bus.register(Box::new(CountingConsumer {
            name: "counter",
            kinds: vec![ExtractionEventKind::RootExtracted],
            count: Arc::clone(&count),
        }));

        let emitted = bus.emit(make_root_discovered());
        assert_eq!(count.load(Ordering::Relaxed), 1, "follow-on event must be routed");
        assert_eq!(emitted.len(), 2, "emitted list must contain seed + follow-on");
    }

    #[test]
    fn test_depth_first_ordering() {
        // Emitter1 listens RootDiscovered → emits [RootExtracted, PassesComplete]
        // Counter counts PassesComplete
        // Depth-first: RootDiscovered → RootExtracted (processed first) → PassesComplete
        let count = Arc::new(AtomicUsize::new(0));
        let mut bus = EventBus::new();

        bus.register(Box::new(EmittingConsumer {
            name: "emitter",
            listens: ExtractionEventKind::RootDiscovered,
            emits: vec![
                make_root_extracted(),
                ExtractionEvent::PassesComplete {
                    slug: "test".into(),
                    nodes: Arc::from(vec![].into_boxed_slice()),
                    edges: Arc::from(vec![].into_boxed_slice()),
                    detected_frameworks: HashSet::new(),
                },
            ],
        }));
        bus.register(Box::new(CountingConsumer {
            name: "counter",
            kinds: vec![ExtractionEventKind::PassesComplete],
            count: Arc::clone(&count),
        }));

        let emitted = bus.emit(make_root_discovered());
        assert_eq!(emitted.len(), 3);
        assert!(matches!(emitted[0], ExtractionEvent::RootDiscovered { .. }));
        assert!(matches!(emitted[1], ExtractionEvent::RootExtracted { .. }));
        assert!(matches!(emitted[2], ExtractionEvent::PassesComplete { .. }));
    }

    #[test]
    fn test_consumer_error_does_not_stop_bus() {
        struct FailingConsumer;
        impl ExtractionConsumer for FailingConsumer {
            fn name(&self) -> &str { "failing" }
            fn subscribes_to(&self) -> &[ExtractionEventKind] {
                &[ExtractionEventKind::RootDiscovered]
            }
            fn on_event(&self, _event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
                Err(anyhow::anyhow!("test error"))
            }
        }

        let count = Arc::new(AtomicUsize::new(0));
        let mut bus = EventBus::new();
        bus.register(Box::new(FailingConsumer));
        bus.register(Box::new(CountingConsumer {
            name: "counter",
            kinds: vec![ExtractionEventKind::RootDiscovered],
            count: Arc::clone(&count),
        }));

        bus.emit(make_root_discovered());
        assert_eq!(count.load(Ordering::Relaxed), 1, "second consumer must still run after first fails");
    }

    #[test]
    fn test_emit_all_processes_all_seeds() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut bus = EventBus::new();
        bus.register(Box::new(CountingConsumer {
            name: "counter",
            kinds: vec![ExtractionEventKind::RootDiscovered],
            count: Arc::clone(&count),
        }));

        bus.emit_all(vec![make_root_discovered(), make_root_discovered(), make_root_discovered()]);
        assert_eq!(count.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn test_multiple_consumers_same_event() {
        let count_a = Arc::new(AtomicUsize::new(0));
        let count_b = Arc::new(AtomicUsize::new(0));
        let mut bus = EventBus::new();
        bus.register(Box::new(CountingConsumer {
            name: "a",
            kinds: vec![ExtractionEventKind::RootDiscovered],
            count: Arc::clone(&count_a),
        }));
        bus.register(Box::new(CountingConsumer {
            name: "b",
            kinds: vec![ExtractionEventKind::RootDiscovered],
            count: Arc::clone(&count_b),
        }));

        bus.emit(make_root_discovered());
        assert_eq!(count_a.load(Ordering::Relaxed), 1);
        assert_eq!(count_b.load(Ordering::Relaxed), 1);
    }

    /// Adversarial: a consumer registered BEFORE the emitter for a follow-on event
    /// must still receive that event (the bus routes ALL events including follow-ons).
    #[test]
    fn test_consumer_receives_follow_on_regardless_of_registration_order() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut bus = EventBus::new();

        // Register counter FIRST (before emitter)
        bus.register(Box::new(CountingConsumer {
            name: "counter",
            kinds: vec![ExtractionEventKind::RootExtracted],
            count: Arc::clone(&count),
        }));
        // Then emitter (produces RootExtracted on RootDiscovered)
        bus.register(Box::new(EmittingConsumer {
            name: "emitter",
            listens: ExtractionEventKind::RootDiscovered,
            emits: vec![make_root_extracted()],
        }));

        bus.emit(make_root_discovered());
        assert_eq!(count.load(Ordering::Relaxed), 1,
            "counter registered before emitter must still receive follow-on event");
    }
}
