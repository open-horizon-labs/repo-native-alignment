//! `ScanStatsConsumer` — live scan state maintained by the event bus.
//!
//! # Purpose
//!
//! `list_roots` previously read `extract_completed.json` WAL sentinels to
//! determine scan status. Sentinels are written only after a phase fully
//! completes and persists — they cannot distinguish "scan in progress" from
//! "scan never ran" and have no visibility into per-language LSP state
//! during enrichment.
//!
//! `ScanStatsConsumer` subscribes to extraction events and maintains live
//! state in an `Arc<RwLock<ScanStats>>`. `list_roots` reads from this
//! singleton for in-progress and complete status. Sentinel files remain as
//! a cold-start fallback (no bus events yet — sentinel exists from a
//! previous run).
//!
//! # Architecture
//!
//! ```text
//! ExtractionEvent            ScanStats mutation
//! ─────────────────────────  ─────────────────────────────────────
//! RootDiscovered             roots_queued += 1
//! RootExtracted              roots_extracted[slug] = RootStats
//! LanguageDetected           languages_in_flight[slug] += [lang]
//! EnrichmentComplete         languages_done[slug] += [lang],
//!                            lsp_edge_counts[slug][lang] = count
//! PassesComplete             roots_complete[slug] = full stats
//! ```
//!
//! All mutations take an exclusive write lock; reads take a shared lock.
//! The lock is uncontested in the common case (only the extraction thread
//! writes; `list_roots` reads from a different task).
//!
//! # Cold-start fallback
//!
//! On cold start (server restarts after a prior scan), no bus events fire.
//! `list_roots` falls back to the sentinel files in `.oh/.cache/` exactly
//! as it did before this change. The `ScanStats` will be empty/default in
//! that case, and the caller must check `roots_complete` being empty as the
//! signal to fall back.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use crate::extract::event_bus::{ExtractionConsumer, ExtractionEvent, ExtractionEventKind};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Per-root stats populated after `RootExtracted`.
#[derive(Debug, Clone)]
pub struct RootExtractedStats {
    /// Number of symbols (nodes) extracted by tree-sitter.
    pub symbol_count: usize,
    /// Number of edges extracted by tree-sitter.
    pub edge_count: usize,
    /// Wall-clock time when extraction completed.
    pub completed_at: Instant,
}

/// Full per-root stats populated after `PassesComplete` (all passes + LSP done).
#[derive(Debug, Clone)]
pub struct RootCompleteStats {
    /// Total symbols after all passes ran.
    pub symbol_count: usize,
    /// Total edges after all passes ran.
    pub edge_count: usize,
    /// Detected framework IDs (e.g. "fastapi", "kafkajs").
    pub detected_frameworks: std::collections::HashSet<String>,
    /// Wall-clock time when all passes completed.
    pub completed_at: Instant,
}

/// Live scan state maintained by [`ScanStatsConsumer`].
///
/// All fields are keyed by root slug. The struct is read-locked by
/// `list_roots` and write-locked by the consumer callbacks.
#[derive(Debug, Default)]
pub struct ScanStats {
    /// How many roots have been queued (received `RootDiscovered`).
    pub roots_queued: usize,

    /// Roots that have completed tree-sitter extraction (`RootExtracted`).
    /// Present between extraction and `PassesComplete`.
    pub roots_extracted: HashMap<String, RootExtractedStats>,

    /// Languages currently in flight (detected but enrichment not yet done).
    /// `LanguageDetected` adds a language; `EnrichmentComplete` removes it.
    pub languages_in_flight: HashMap<String, Vec<String>>,

    /// Languages for which enrichment has completed, per root.
    pub languages_done: HashMap<String, Vec<String>>,

    /// LSP edge counts per root per language, populated on `EnrichmentComplete`.
    pub lsp_edge_counts: HashMap<String, HashMap<String, usize>>,

    /// Roots that have completed all passes (`PassesComplete`).
    pub roots_complete: HashMap<String, RootCompleteStats>,
}

impl ScanStats {
    /// Returns true if any scan activity has been observed (at least one root
    /// discovered). When false, `list_roots` should fall back to sentinel files.
    pub fn has_activity(&self) -> bool {
        self.roots_queued > 0
    }

    /// Returns `true` if the given root has finished all post-extraction passes.
    pub fn is_root_complete(&self, slug: &str) -> bool {
        self.roots_complete.contains_key(slug)
    }

    /// Returns `true` if the given root is currently being extracted or enriched
    /// (seen via `RootDiscovered` but `PassesComplete` not yet received).
    pub fn is_root_in_progress(&self, slug: &str) -> bool {
        self.roots_queued > 0
            && !self.roots_complete.contains_key(slug)
    }
}

// ---------------------------------------------------------------------------
// ScanStatsConsumer
// ---------------------------------------------------------------------------

/// Singleton consumer that maintains live scan state.
///
/// Register via [`build_builtin_bus`] before emitting any events.
/// Obtain the shared state via [`ScanStatsConsumer::stats`].
///
/// [`build_builtin_bus`]: crate::extract::consumers::build_builtin_bus
pub struct ScanStatsConsumer {
    /// Shared mutable scan state. The bus and any reader share this Arc.
    pub stats: Arc<RwLock<ScanStats>>,
}

impl ScanStatsConsumer {
    /// Create a new consumer backed by a fresh, empty [`ScanStats`].
    pub fn new() -> Self {
        Self {
            stats: Arc::new(RwLock::new(ScanStats::default())),
        }
    }

    /// Return a clone of the `Arc` so callers can hold a handle to the stats
    /// without owning the consumer.
    pub fn stats_handle(&self) -> Arc<RwLock<ScanStats>> {
        Arc::clone(&self.stats)
    }
}

impl Default for ScanStatsConsumer {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtractionConsumer for ScanStatsConsumer {
    fn name(&self) -> &str {
        "scan_stats"
    }

    fn subscribes_to(&self) -> &[ExtractionEventKind] {
        &[
            ExtractionEventKind::RootDiscovered,
            ExtractionEventKind::RootExtracted,
            ExtractionEventKind::LanguageDetected,
            ExtractionEventKind::EnrichmentComplete,
            ExtractionEventKind::PassesComplete,
        ]
    }

    fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        // Acquire an exclusive write lock. The lock is short-lived (a few HashMap ops)
        // and uncontested in normal operation — the extraction thread is the only writer.
        let mut stats = self.stats.write().map_err(|e| anyhow::anyhow!("ScanStatsConsumer lock poisoned: {}", e))?;

        match event {
            ExtractionEvent::RootDiscovered { slug, .. } => {
                stats.roots_queued += 1;
                tracing::debug!(
                    "ScanStatsConsumer: RootDiscovered '{}' (total queued: {})",
                    slug,
                    stats.roots_queued,
                );
            }

            ExtractionEvent::RootExtracted { slug, nodes, edges, .. } => {
                stats.roots_extracted.insert(slug.clone(), RootExtractedStats {
                    symbol_count: nodes.len(),
                    edge_count: edges.len(),
                    completed_at: Instant::now(),
                });
                tracing::debug!(
                    "ScanStatsConsumer: RootExtracted '{}': {} symbols, {} edges",
                    slug,
                    nodes.len(),
                    edges.len(),
                );
            }

            ExtractionEvent::LanguageDetected { slug, language, .. } => {
                stats.languages_in_flight
                    .entry(slug.clone())
                    .or_default()
                    .push(language.clone());
                tracing::debug!(
                    "ScanStatsConsumer: LanguageDetected '{}' in '{}'",
                    language,
                    slug,
                );
            }

            ExtractionEvent::EnrichmentComplete { slug, language, added_edges, .. } => {
                // Remove from in-flight.
                if let Some(in_flight) = stats.languages_in_flight.get_mut(slug) {
                    in_flight.retain(|l| l != language);
                }
                // Add to done.
                stats.languages_done
                    .entry(slug.clone())
                    .or_default()
                    .push(language.clone());
                // Record LSP edge count.
                stats.lsp_edge_counts
                    .entry(slug.clone())
                    .or_default()
                    .insert(language.clone(), added_edges.len());
                tracing::debug!(
                    "ScanStatsConsumer: EnrichmentComplete '{}' for '{}': {} LSP edges",
                    language,
                    slug,
                    added_edges.len(),
                );
            }

            ExtractionEvent::PassesComplete { slug, nodes, edges, detected_frameworks, .. } => {
                stats.roots_complete.insert(slug.clone(), RootCompleteStats {
                    symbol_count: nodes.len(),
                    edge_count: edges.len(),
                    detected_frameworks: detected_frameworks.clone(),
                    completed_at: Instant::now(),
                });
                // Clean up intermediate state now that this root is complete.
                stats.roots_extracted.remove(slug);
                stats.languages_in_flight.remove(slug);
                tracing::info!(
                    "ScanStatsConsumer: PassesComplete '{}': {} symbols, {} edges, {} framework(s)",
                    slug,
                    nodes.len(),
                    edges.len(),
                    detected_frameworks.len(),
                );
            }

            // Other events not subscribed to — guard clause in bus ensures we never
            // receive them, but keep exhaustive for correctness.
            _ => {}
        }

        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::event_bus::ExtractionEvent;
    use std::path::PathBuf;

    fn make_consumer() -> ScanStatsConsumer {
        ScanStatsConsumer::new()
    }

    fn slug(s: &str) -> String {
        s.to_string()
    }

    fn empty_arc_nodes() -> std::sync::Arc<[crate::graph::Node]> {
        std::sync::Arc::from([])
    }

    fn empty_arc_edges() -> std::sync::Arc<[crate::graph::Edge]> {
        std::sync::Arc::from([])
    }

    #[test]
    fn test_root_discovered_increments_queued() {
        let c = make_consumer();
        let event = ExtractionEvent::RootDiscovered {
            slug: slug("api"),
            path: PathBuf::from("."),
            lsp_only: false,
        };
        c.on_event(&event).unwrap();

        let stats = c.stats.read().unwrap();
        assert_eq!(stats.roots_queued, 1);
        assert!(stats.has_activity());
    }

    #[test]
    fn test_multiple_roots_discovered() {
        let c = make_consumer();
        for name in ["api", "frontend", "infra"] {
            c.on_event(&ExtractionEvent::RootDiscovered {
                slug: slug(name),
                path: PathBuf::from("."),
                lsp_only: false,
            }).unwrap();
        }
        let stats = c.stats.read().unwrap();
        assert_eq!(stats.roots_queued, 3);
    }

    #[test]
    fn test_root_extracted_records_stats() {
        let c = make_consumer();
        // RootDiscovered first
        c.on_event(&ExtractionEvent::RootDiscovered {
            slug: slug("api"),
            path: PathBuf::from("."),
            lsp_only: false,
        }).unwrap();
        // Then RootExtracted
        c.on_event(&ExtractionEvent::RootExtracted {
            slug: slug("api"),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from(vec![].into_boxed_slice()),
            edges: std::sync::Arc::from(vec![].into_boxed_slice()),
        }).unwrap();
        let stats = c.stats.read().unwrap();
        let extracted = stats.roots_extracted.get("api").expect("api should be in roots_extracted");
        assert_eq!(extracted.symbol_count, 0);
        assert_eq!(extracted.edge_count, 0);
        assert!(stats.is_root_in_progress("api"), "root should be in-progress after RootExtracted");
    }

    #[test]
    fn test_language_detected_adds_to_in_flight() {
        let c = make_consumer();
        c.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug("api"),
            language: "rust".into(),
            nodes: empty_arc_nodes(),
        }).unwrap();
        c.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug("api"),
            language: "python".into(),
            nodes: empty_arc_nodes(),
        }).unwrap();
        let stats = c.stats.read().unwrap();
        let in_flight = stats.languages_in_flight.get("api").expect("api should have in-flight langs");
        assert!(in_flight.contains(&"rust".to_string()));
        assert!(in_flight.contains(&"python".to_string()));
    }

    #[test]
    fn test_enrichment_complete_moves_lang_from_in_flight_to_done() {
        let c = make_consumer();
        // Add rust to in-flight
        c.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug("api"),
            language: "rust".into(),
            nodes: empty_arc_nodes(),
        }).unwrap();
        // Complete enrichment for rust
        c.on_event(&ExtractionEvent::EnrichmentComplete {
            slug: slug("api"),
            language: "rust".into(),
            added_edges: empty_arc_edges(),
            new_nodes: empty_arc_nodes(),
            updated_nodes: std::sync::Arc::from([]),
        }).unwrap();
        let stats = c.stats.read().unwrap();
        let in_flight = stats.languages_in_flight.get("api").map(|v| v.as_slice()).unwrap_or(&[]);
        assert!(!in_flight.contains(&"rust".to_string()), "rust should no longer be in-flight");
        let done = stats.languages_done.get("api").expect("api should have done langs");
        assert!(done.contains(&"rust".to_string()), "rust should be in done langs");
    }

    #[test]
    fn test_enrichment_complete_records_lsp_edge_count() {
        use crate::graph::{Edge, EdgeKind, ExtractionSource, Confidence, NodeId, NodeKind};
        use std::path::PathBuf as Pb;
        let c = make_consumer();

        // Build 3 fake edges
        let make_edge = |n: &str| Edge {
            from: NodeId { root: "api".into(), file: Pb::from("a.rs"), name: n.into(), kind: NodeKind::Function },
            to: NodeId { root: "api".into(), file: Pb::from("b.rs"), name: "b".into(), kind: NodeKind::Function },
            kind: EdgeKind::Calls,
            source: ExtractionSource::TreeSitter,
            confidence: Confidence::Confirmed,
        };
        let edges: Vec<Edge> = (0..3).map(|i| make_edge(&format!("fn{}", i))).collect();

        c.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug("api"),
            language: "rust".into(),
            nodes: empty_arc_nodes(),
        }).unwrap();
        c.on_event(&ExtractionEvent::EnrichmentComplete {
            slug: slug("api"),
            language: "rust".into(),
            added_edges: std::sync::Arc::from(edges.into_boxed_slice()),
            new_nodes: empty_arc_nodes(),
            updated_nodes: std::sync::Arc::from([]),
        }).unwrap();

        let stats = c.stats.read().unwrap();
        let count = stats.lsp_edge_counts
            .get("api").expect("api")
            .get("rust").copied().unwrap_or(0);
        assert_eq!(count, 3, "lsp_edge_count should reflect added_edges length");
    }

    #[test]
    fn test_passes_complete_marks_root_complete_and_clears_intermediate() {
        let c = make_consumer();
        // Simulate full pipeline for "api"
        c.on_event(&ExtractionEvent::RootDiscovered {
            slug: slug("api"), path: PathBuf::from("."), lsp_only: false,
        }).unwrap();
        c.on_event(&ExtractionEvent::RootExtracted {
            slug: slug("api"), path: PathBuf::from("."),
            nodes: std::sync::Arc::from(vec![].into_boxed_slice()),
            edges: std::sync::Arc::from(vec![].into_boxed_slice()),
        }).unwrap();
        c.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug("api"), language: "rust".into(), nodes: empty_arc_nodes(),
        }).unwrap();
        c.on_event(&ExtractionEvent::EnrichmentComplete {
            slug: slug("api"), language: "rust".into(),
            added_edges: empty_arc_edges(), new_nodes: empty_arc_nodes(),
            updated_nodes: std::sync::Arc::from([]),
        }).unwrap();
        c.on_event(&ExtractionEvent::PassesComplete {
            slug: slug("api"),
            nodes: std::sync::Arc::from(vec![].into_boxed_slice()),
            edges: std::sync::Arc::from(vec![].into_boxed_slice()),
            detected_frameworks: std::collections::HashSet::new(),
        }).unwrap();

        let stats = c.stats.read().unwrap();
        assert!(stats.is_root_complete("api"), "root should be complete after PassesComplete");
        assert!(!stats.is_root_in_progress("api"), "root should not be in-progress after PassesComplete");
        assert!(!stats.roots_extracted.contains_key("api"), "intermediate extracted state should be removed");
        assert!(!stats.languages_in_flight.contains_key("api"), "in-flight languages should be cleared");
    }

    #[test]
    fn test_no_activity_when_empty() {
        let c = make_consumer();
        let stats = c.stats.read().unwrap();
        assert!(!stats.has_activity(), "fresh consumer has no activity");
        assert!(!stats.is_root_complete("anything"));
        assert!(!stats.is_root_in_progress("anything"));
    }

    /// Verify that stats_handle() and the consumer share the same allocation.
    #[test]
    fn test_stats_handle_shares_same_arc() {
        let c = make_consumer();
        let handle = c.stats_handle();
        // Mutate through the consumer's internal Arc
        c.on_event(&ExtractionEvent::RootDiscovered {
            slug: slug("test"), path: PathBuf::from("."), lsp_only: false,
        }).unwrap();
        // Read through the handle — must see the mutation
        let stats = handle.read().unwrap();
        assert_eq!(stats.roots_queued, 1, "stats_handle must share the same Arc as the consumer");
    }

    /// Adversarial: EnrichmentComplete for a language that was never in-flight
    /// must not panic and must still add the language to done.
    #[test]
    fn test_enrichment_complete_without_prior_language_detected() {
        let c = make_consumer();
        // No LanguageDetected fired before this
        let result = c.on_event(&ExtractionEvent::EnrichmentComplete {
            slug: slug("api"),
            language: "go".into(),
            added_edges: empty_arc_edges(),
            new_nodes: empty_arc_nodes(),
            updated_nodes: std::sync::Arc::from([]),
        });
        assert!(result.is_ok(), "should not fail even without prior LanguageDetected");
        let stats = c.stats.read().unwrap();
        let done = stats.languages_done.get("api").expect("api should be in done");
        assert!(done.contains(&"go".to_string()));
    }

    /// Adversarial: PassesComplete before RootDiscovered — must not panic.
    #[test]
    fn test_passes_complete_without_prior_discovered() {
        let c = make_consumer();
        let result = c.on_event(&ExtractionEvent::PassesComplete {
            slug: slug("api"),
            nodes: std::sync::Arc::from(vec![].into_boxed_slice()),
            edges: std::sync::Arc::from(vec![].into_boxed_slice()),
            detected_frameworks: std::collections::HashSet::new(),
        });
        assert!(result.is_ok(), "PassesComplete without prior events must not panic");
        // Root is complete despite no prior events
        let stats = c.stats.read().unwrap();
        assert!(stats.roots_complete.contains_key("api"));
        // has_activity is still false (roots_queued == 0)
        assert!(!stats.has_activity(), "roots_queued not incremented — no RootDiscovered fired");
    }
}
