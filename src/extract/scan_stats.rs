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
//!                            lsp_edge_counts[slug][lang] = count,
//!                            lsp_stats[slug][lang] = LspLanguageStats
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
use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::extract::event_bus::{ExtractionConsumer, ExtractionEvent, ExtractionEventKind};

// ---------------------------------------------------------------------------
// Per-language LSP enrichment stats  (#575)
// ---------------------------------------------------------------------------

/// Status of a per-language LSP enrichment attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspStatus {
    Ok,
    Aborted,
    NotFound,
    Failed,
}

/// Durable evidence that readiness validation processed a symbol request,
/// observed an authoritative quiescence signal, or never reached a supported
/// validation method. `symbol_count: Some(0)` is a successful processed-zero
/// result; the status distinguishes quiescence from not-validated when no
/// symbol count applies.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspValidationStatus {
    Processed,
    Quiescent,
    NotValidated,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LspValidationEvidence {
    pub language: String,
    pub server_name: String,
    pub status: LspValidationStatus,
    pub method: Option<String>,
    pub symbol_count: Option<usize>,
    pub detail: Option<String>,
}

impl LspValidationEvidence {
    pub fn processed(
        language: impl Into<String>,
        server_name: impl Into<String>,
        method: impl Into<String>,
        symbol_count: usize,
    ) -> Self {
        Self {
            language: language.into(),
            server_name: server_name.into(),
            status: LspValidationStatus::Processed,
            method: Some(method.into()),
            symbol_count: Some(symbol_count),
            detail: None,
        }
    }

    pub fn not_validated(
        language: impl Into<String>,
        server_name: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            language: language.into(),
            server_name: server_name.into(),
            status: LspValidationStatus::NotValidated,
            method: None,
            symbol_count: None,
            detail: Some(detail.into()),
        }
    }

    pub fn quiescent(
        language: impl Into<String>,
        server_name: impl Into<String>,
        method: impl Into<String>,
    ) -> Self {
        Self {
            language: language.into(),
            server_name: server_name.into(),
            status: LspValidationStatus::Quiescent,
            method: Some(method.into()),
            symbol_count: None,
            detail: None,
        }
    }

    pub fn summary(&self) -> String {
        match (&self.status, self.method.as_deref(), self.symbol_count) {
            (LspValidationStatus::Processed, Some(method), Some(count)) => {
                format!("processed via {method} ({count} symbols)")
            }
            (LspValidationStatus::Quiescent, Some(method), None) => {
                format!("quiescent via {method}")
            }
            _ => format!(
                "not validated{}",
                self.detail
                    .as_deref()
                    .map(|detail| format!(": {detail}"))
                    .unwrap_or_default()
            ),
        }
    }
}

impl std::fmt::Display for LspStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LspStatus::Ok => write!(f, "OK"),
            LspStatus::Aborted => write!(f, "ABORTED"),
            LspStatus::NotFound => write!(f, "not found"),
            LspStatus::Failed => write!(f, "FAILED"),
        }
    }
}

/// Per-language LSP enrichment statistics for the scan summary.
#[derive(Debug, Clone)]
pub struct LspEnrichmentEntry {
    pub language: String,
    pub server_name: String,
    pub root_hint: Option<String>,
    pub edge_count: usize,
    pub node_count: usize,
    pub error_count: usize,
    pub duration: Duration,
    pub status: LspStatus,
    pub remediation: Option<String>,
    pub query_metrics: Vec<crate::extract::lsp::LspQueryMetric>,
    pub validation: Option<LspValidationEvidence>,
}

impl LspEnrichmentEntry {
    /// Render a single line for the scan summary.
    pub fn summary_line(&self) -> String {
        let root_part = if let Some(ref root) = self.root_hint {
            format!(" -> {}/", root)
        } else {
            String::new()
        };
        let label = format!("  {} ({}){}", self.language, self.server_name, root_part);
        let mut line = match self.status {
            LspStatus::NotFound => format!("{:<50} {}", label, self.status),
            LspStatus::Ok => {
                let mut line = format!(
                    "{:<50} {} ({} edges, {} nodes, {:.1}s)",
                    label,
                    self.status,
                    self.edge_count,
                    self.node_count,
                    self.duration.as_secs_f64()
                );
                if self.edge_count == 0 {
                    line.push_str(" -- no edges");
                }
                line
            }
            LspStatus::Aborted | LspStatus::Failed => {
                format!(
                    "{:<50} {} ({} edges, {} errors, {:.1}s)",
                    label,
                    self.status,
                    self.edge_count,
                    self.error_count,
                    self.duration.as_secs_f64()
                )
            }
        };
        if let Some(remediation) = &self.remediation
            && self.status != LspStatus::Ok
        {
            line.push_str(" -- ");
            line.push_str(remediation);
        }
        if !self.query_metrics.is_empty() {
            let scheduled = self
                .query_metrics
                .iter()
                .map(|metric| metric.scheduled_requests)
                .sum::<usize>();
            let non_empty = self
                .query_metrics
                .iter()
                .map(|metric| metric.non_empty_responses)
                .sum::<usize>();
            line.push_str(&format!(
                " -- query yield {non_empty}/{scheduled} non-empty"
            ));
        }
        if let Some(validation) = &self.validation {
            line.push_str(" -- validation ");
            line.push_str(&validation.summary());
        }
        line
    }
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Per-language LSP enrichment statistics for a single root.
///
/// Populated from `EnrichmentComplete` events and the metadata they carry.
/// Rendered by `list_roots` to show per-root LSP effectiveness at a glance.
#[derive(Debug, Clone)]
pub struct LspLanguageStats {
    /// LSP server binary name (e.g., "rust-analyzer", "pyright-langserver").
    pub server_name: String,
    /// Number of edges produced by LSP enrichment.
    pub edge_count: usize,
    /// Number of virtual nodes produced by LSP enrichment (e.g., external callee stubs).
    pub node_count: usize,
    /// Wall-clock duration of the enrichment.
    pub duration: Duration,
    /// Number of errors encountered during enrichment.
    pub error_count: usize,
    /// Whether the enrichment was aborted (e.g., too many errors, timeout).
    pub aborted: bool,
    /// Whether the server binary was missing before enrichment could start.
    pub server_missing: bool,
    /// Optional actionable setup guidance for this language server.
    pub remediation: Option<String>,
    /// Query-yield measurements grouped by operation and declaration class.
    pub query_metrics: Vec<crate::extract::lsp::LspQueryMetric>,
    /// Capability-driven readiness evidence. `Some(0)` symbols means the
    /// server processed the validation request successfully and found none;
    /// authoritative quiescence signals do not fabricate a symbol count.
    pub validation: Option<LspValidationEvidence>,
}

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

    /// Start times for in-flight language enrichments, for duration tracking.
    /// `LanguageDetected` records `Instant::now()`; `EnrichmentComplete` reads and removes.
    pub(crate) enrichment_start_times: HashMap<String, HashMap<String, Instant>>,

    /// Languages for which enrichment has completed, per root.
    pub languages_done: HashMap<String, Vec<String>>,

    /// LSP edge counts per root per language, populated on `EnrichmentComplete`.
    pub lsp_edge_counts: HashMap<String, HashMap<String, usize>>,

    /// Roots that have completed all passes (`PassesComplete`).
    pub roots_complete: HashMap<String, RootCompleteStats>,

    /// Per-root, per-language LSP enrichment stats.
    /// Populated on `EnrichmentComplete` when the event carries LSP metadata.
    pub lsp_stats: HashMap<String, HashMap<String, LspLanguageStats>>,

    /// Per-root encoding statistics: files skipped as binary or lossy-decoded.
    /// Populated by callers after extraction (not via the event bus).
    pub encoding_stats: HashMap<String, crate::extract::EncodingStats>,
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
        self.roots_queued > 0 && !self.roots_complete.contains_key(slug)
    }

    /// Replace encoding stats for a root (used by full-scan paths that process
    /// every file and produce a complete picture).
    pub fn set_encoding_stats(&mut self, slug: &str, stats: crate::extract::EncodingStats) {
        if stats.is_empty() {
            self.encoding_stats.remove(slug);
        } else {
            self.encoding_stats.insert(slug.to_string(), stats);
        }
    }

    /// Merge encoding stats from an incremental scan into the existing totals
    /// for a root. Adds the delta counts to whatever is already stored.
    pub fn merge_encoding_stats(&mut self, slug: &str, delta: &crate::extract::EncodingStats) {
        if delta.is_empty() {
            return;
        }
        self.encoding_stats
            .entry(slug.to_string())
            .or_default()
            .merge(delta);
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

#[async_trait]
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
            ExtractionEventKind::LspQueryMetrics,
            ExtractionEventKind::PassesComplete,
        ]
    }

    async fn on_event(&self, event: &ExtractionEvent) -> anyhow::Result<Vec<ExtractionEvent>> {
        // Acquire an exclusive write lock. The lock is short-lived (a few HashMap ops)
        // and uncontested in normal operation — the extraction thread is the only writer.
        let mut stats = self
            .stats
            .write()
            .map_err(|e| anyhow::anyhow!("ScanStatsConsumer lock poisoned: {}", e))?;

        match event {
            ExtractionEvent::RootDiscovered { slug, .. } => {
                stats.roots_queued += 1;
                tracing::debug!(
                    "ScanStatsConsumer: RootDiscovered '{}' (total queued: {})",
                    slug,
                    stats.roots_queued,
                );
            }

            ExtractionEvent::RootExtracted {
                slug, nodes, edges, ..
            } => {
                stats.roots_extracted.insert(
                    slug.clone(),
                    RootExtractedStats {
                        symbol_count: nodes.len(),
                        edge_count: edges.len(),
                        completed_at: Instant::now(),
                    },
                );
                tracing::debug!(
                    "ScanStatsConsumer: RootExtracted '{}': {} symbols, {} edges",
                    slug,
                    nodes.len(),
                    edges.len(),
                );
            }

            ExtractionEvent::LanguageDetected { slug, language, .. } => {
                stats
                    .languages_in_flight
                    .entry(slug.clone())
                    .or_default()
                    .push(language.clone());
                // Record start time for duration tracking.
                stats
                    .enrichment_start_times
                    .entry(slug.clone())
                    .or_default()
                    .insert(language.clone(), Instant::now());
                tracing::debug!(
                    "ScanStatsConsumer: LanguageDetected '{}' in '{}'",
                    language,
                    slug,
                );
            }

            ExtractionEvent::EnrichmentComplete {
                slug,
                language,
                added_edges,
                new_nodes,
                server_name,
                error_count,
                aborted,
                server_missing,
                remediation,
                validation,
                ..
            } => {
                // Remove from in-flight.
                if let Some(in_flight) = stats.languages_in_flight.get_mut(slug) {
                    in_flight.retain(|l| l != language);
                }
                // Add to done.
                stats
                    .languages_done
                    .entry(slug.clone())
                    .or_default()
                    .push(language.clone());
                // Record LSP edge count (backward compat).
                stats
                    .lsp_edge_counts
                    .entry(slug.clone())
                    .or_default()
                    .insert(language.clone(), added_edges.len());
                // Record full LSP stats when server_name is present.
                let duration = stats
                    .enrichment_start_times
                    .get(slug)
                    .and_then(|m| m.get(language))
                    .map(|start| start.elapsed())
                    .unwrap_or_default();
                // Clean up start time.
                if let Some(starts) = stats.enrichment_start_times.get_mut(slug) {
                    starts.remove(language);
                }
                if let Some(name) = server_name {
                    stats.lsp_stats.entry(slug.clone()).or_default().insert(
                        language.clone(),
                        LspLanguageStats {
                            server_name: name.clone(),
                            edge_count: added_edges.len(),
                            node_count: new_nodes.len(),
                            duration,
                            error_count: *error_count,
                            aborted: *aborted,
                            server_missing: *server_missing,
                            remediation: remediation.clone(),
                            query_metrics: Vec::new(),
                            validation: validation.clone(),
                        },
                    );
                }
                tracing::debug!(
                    "ScanStatsConsumer: EnrichmentComplete '{}' for '{}': {} LSP edges, validation={}",
                    language,
                    slug,
                    added_edges.len(),
                    validation
                        .as_ref()
                        .map(LspValidationEvidence::summary)
                        .unwrap_or_else(|| "not recorded".to_string()),
                );
            }

            ExtractionEvent::LspQueryMetrics {
                slug,
                language,
                metrics,
            } => {
                if let Some(language_stats) = stats
                    .lsp_stats
                    .get_mut(slug)
                    .and_then(|by_language| by_language.get_mut(language))
                {
                    language_stats.query_metrics = metrics.to_vec();
                }
            }

            ExtractionEvent::PassesComplete {
                slug,
                nodes,
                edges,
                detected_frameworks,
                ..
            } => {
                stats.roots_complete.insert(
                    slug.clone(),
                    RootCompleteStats {
                        symbol_count: nodes.len(),
                        edge_count: edges.len(),
                        detected_frameworks: detected_frameworks.clone(),
                        completed_at: Instant::now(),
                    },
                );
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

    /// `ScanStatsConsumer` is stateful — it accumulates scan metrics across events.
    /// The bus must not cache its output; every `on_event` call must reach it.
    fn is_cacheable(&self) -> bool {
        false
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

    #[tokio::test]
    async fn test_root_discovered_increments_queued() {
        let c = make_consumer();
        let event = ExtractionEvent::RootDiscovered {
            slug: slug("api"),
            path: PathBuf::from("."),
            lsp_only: false,
        };
        c.on_event(&event).await.unwrap();

        let stats = c.stats.read().unwrap();
        assert_eq!(stats.roots_queued, 1);
        assert!(stats.has_activity());
    }

    #[tokio::test]
    async fn test_multiple_roots_discovered() {
        let c = make_consumer();
        for name in ["api", "frontend", "infra"] {
            c.on_event(&ExtractionEvent::RootDiscovered {
                slug: slug(name),
                path: PathBuf::from("."),
                lsp_only: false,
            })
            .await
            .unwrap();
        }
        let stats = c.stats.read().unwrap();
        assert_eq!(stats.roots_queued, 3);
    }

    #[tokio::test]
    async fn test_root_extracted_records_stats() {
        let c = make_consumer();
        // RootDiscovered first
        c.on_event(&ExtractionEvent::RootDiscovered {
            slug: slug("api"),
            path: PathBuf::from("."),
            lsp_only: false,
        })
        .await
        .unwrap();
        // Then RootExtracted
        c.on_event(&ExtractionEvent::RootExtracted {
            slug: slug("api"),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from(vec![].into_boxed_slice()),
            edges: std::sync::Arc::from(vec![].into_boxed_slice()),
            dirty_slugs: None,
        })
        .await
        .unwrap();
        let stats = c.stats.read().unwrap();
        let extracted = stats
            .roots_extracted
            .get("api")
            .expect("api should be in roots_extracted");
        assert_eq!(extracted.symbol_count, 0);
        assert_eq!(extracted.edge_count, 0);
        assert!(
            stats.is_root_in_progress("api"),
            "root should be in-progress after RootExtracted"
        );
    }

    #[tokio::test]
    async fn test_language_detected_adds_to_in_flight() {
        let c = make_consumer();
        c.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug("api"),
            language: "rust".into(),
            nodes: empty_arc_nodes(),
        })
        .await
        .unwrap();
        c.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug("api"),
            language: "python".into(),
            nodes: empty_arc_nodes(),
        })
        .await
        .unwrap();
        let stats = c.stats.read().unwrap();
        let in_flight = stats
            .languages_in_flight
            .get("api")
            .expect("api should have in-flight langs");
        assert!(in_flight.contains(&"rust".to_string()));
        assert!(in_flight.contains(&"python".to_string()));
    }

    #[tokio::test]
    async fn test_enrichment_complete_moves_lang_from_in_flight_to_done() {
        let c = make_consumer();
        // Add rust to in-flight
        c.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug("api"),
            language: "rust".into(),
            nodes: empty_arc_nodes(),
        })
        .await
        .unwrap();
        // Complete enrichment for rust
        c.on_event(&ExtractionEvent::EnrichmentComplete {
            slug: slug("api"),
            language: "rust".into(),
            added_edges: empty_arc_edges(),
            new_nodes: empty_arc_nodes(),
            updated_nodes: std::sync::Arc::from([]),
            server_name: None,
            error_count: 0,
            server_missing: false,
            remediation: None,
            aborted: false,
            diagnostic: None,
            validation: None,
        })
        .await
        .unwrap();
        let stats = c.stats.read().unwrap();
        let in_flight = stats
            .languages_in_flight
            .get("api")
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        assert!(
            !in_flight.contains(&"rust".to_string()),
            "rust should no longer be in-flight"
        );
        let done = stats
            .languages_done
            .get("api")
            .expect("api should have done langs");
        assert!(
            done.contains(&"rust".to_string()),
            "rust should be in done langs"
        );
    }

    #[tokio::test]
    async fn test_enrichment_complete_records_lsp_edge_count() {
        use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, NodeId, NodeKind};
        use std::path::PathBuf as Pb;
        let c = make_consumer();

        // Build 3 fake edges
        let make_edge = |n: &str| Edge {
            from: NodeId {
                root: "api".into(),
                file: Pb::from("a.rs"),
                name: n.into(),
                kind: NodeKind::Function,
            },
            to: NodeId {
                root: "api".into(),
                file: Pb::from("b.rs"),
                name: "b".into(),
                kind: NodeKind::Function,
            },
            kind: EdgeKind::Calls,
            source: ExtractionSource::TreeSitter,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let edges: Vec<Edge> = (0..3).map(|i| make_edge(&format!("fn{}", i))).collect();

        c.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug("api"),
            language: "rust".into(),
            nodes: empty_arc_nodes(),
        })
        .await
        .unwrap();
        c.on_event(&ExtractionEvent::EnrichmentComplete {
            slug: slug("api"),
            language: "rust".into(),
            added_edges: std::sync::Arc::from(edges.into_boxed_slice()),
            new_nodes: empty_arc_nodes(),
            updated_nodes: std::sync::Arc::from([]),
            server_name: Some("rust-analyzer".to_string()),
            error_count: 0,
            server_missing: false,
            remediation: None,
            aborted: false,
            diagnostic: None,
            validation: Some(LspValidationEvidence::processed(
                "rust",
                "rust-analyzer",
                "workspace/symbol",
                0,
            )),
        })
        .await
        .unwrap();

        let stats = c.stats.read().unwrap();
        let count = stats
            .lsp_edge_counts
            .get("api")
            .expect("api")
            .get("rust")
            .copied()
            .unwrap_or(0);
        assert_eq!(count, 3, "lsp_edge_count should reflect added_edges length");
        let validation = stats.lsp_stats["api"]["rust"]
            .validation
            .as_ref()
            .expect("validation evidence should be delivered");
        assert_eq!(validation.status, LspValidationStatus::Processed);
        assert_eq!(validation.symbol_count, Some(0));
    }

    #[tokio::test]
    async fn test_passes_complete_marks_root_complete_and_clears_intermediate() {
        let c = make_consumer();
        // Simulate full pipeline for "api"
        c.on_event(&ExtractionEvent::RootDiscovered {
            slug: slug("api"),
            path: PathBuf::from("."),
            lsp_only: false,
        })
        .await
        .unwrap();
        c.on_event(&ExtractionEvent::RootExtracted {
            slug: slug("api"),
            path: PathBuf::from("."),
            nodes: std::sync::Arc::from(vec![].into_boxed_slice()),
            edges: std::sync::Arc::from(vec![].into_boxed_slice()),
            dirty_slugs: None,
        })
        .await
        .unwrap();
        c.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug("api"),
            language: "rust".into(),
            nodes: empty_arc_nodes(),
        })
        .await
        .unwrap();
        c.on_event(&ExtractionEvent::EnrichmentComplete {
            slug: slug("api"),
            language: "rust".into(),
            added_edges: empty_arc_edges(),
            new_nodes: empty_arc_nodes(),
            updated_nodes: std::sync::Arc::from([]),
            server_name: None,
            error_count: 0,
            server_missing: false,
            remediation: None,
            aborted: false,
            diagnostic: None,
            validation: None,
        })
        .await
        .unwrap();
        c.on_event(&ExtractionEvent::PassesComplete {
            slug: slug("api"),
            nodes: std::sync::Arc::from(vec![].into_boxed_slice()),
            edges: std::sync::Arc::from(vec![].into_boxed_slice()),
            detected_frameworks: std::collections::HashSet::new(),
            enrichment_diagnostics: std::sync::Arc::from([]),
        })
        .await
        .unwrap();

        let stats = c.stats.read().unwrap();
        assert!(
            stats.is_root_complete("api"),
            "root should be complete after PassesComplete"
        );
        assert!(
            !stats.is_root_in_progress("api"),
            "root should not be in-progress after PassesComplete"
        );
        assert!(
            !stats.roots_extracted.contains_key("api"),
            "intermediate extracted state should be removed"
        );
        assert!(
            !stats.languages_in_flight.contains_key("api"),
            "in-flight languages should be cleared"
        );
    }

    #[tokio::test]
    async fn test_no_activity_when_empty() {
        let c = make_consumer();
        let stats = c.stats.read().unwrap();
        assert!(!stats.has_activity(), "fresh consumer has no activity");
        assert!(!stats.is_root_complete("anything"));
        assert!(!stats.is_root_in_progress("anything"));
    }

    /// Verify that stats_handle() and the consumer share the same allocation.
    #[tokio::test]
    async fn test_stats_handle_shares_same_arc() {
        let c = make_consumer();
        let handle = c.stats_handle();
        // Mutate through the consumer's internal Arc
        c.on_event(&ExtractionEvent::RootDiscovered {
            slug: slug("test"),
            path: PathBuf::from("."),
            lsp_only: false,
        })
        .await
        .unwrap();
        // Read through the handle — must see the mutation
        let stats = handle.read().unwrap();
        assert_eq!(
            stats.roots_queued, 1,
            "stats_handle must share the same Arc as the consumer"
        );
    }

    /// Adversarial: EnrichmentComplete for a language that was never in-flight
    /// must not panic and must still add the language to done.
    #[tokio::test]
    async fn test_enrichment_complete_without_prior_language_detected() {
        let c = make_consumer();
        // No LanguageDetected fired before this
        let result = c
            .on_event(&ExtractionEvent::EnrichmentComplete {
                slug: slug("api"),
                language: "go".into(),
                added_edges: empty_arc_edges(),
                new_nodes: empty_arc_nodes(),
                updated_nodes: std::sync::Arc::from([]),
                server_name: None,
                error_count: 0,
                server_missing: false,
                remediation: None,
                aborted: false,
                diagnostic: None,
                validation: None,
            })
            .await;
        assert!(
            result.is_ok(),
            "should not fail even without prior LanguageDetected"
        );
        let stats = c.stats.read().unwrap();
        let done = stats
            .languages_done
            .get("api")
            .expect("api should be in done");
        assert!(done.contains(&"go".to_string()));
    }

    /// Adversarial: PassesComplete before RootDiscovered — must not panic.
    #[tokio::test]
    async fn test_passes_complete_without_prior_discovered() {
        let c = make_consumer();
        let result = c
            .on_event(&ExtractionEvent::PassesComplete {
                slug: slug("api"),
                nodes: std::sync::Arc::from(vec![].into_boxed_slice()),
                edges: std::sync::Arc::from(vec![].into_boxed_slice()),
                detected_frameworks: std::collections::HashSet::new(),
                enrichment_diagnostics: std::sync::Arc::from([]),
            })
            .await;
        assert!(
            result.is_ok(),
            "PassesComplete without prior events must not panic"
        );
        // Root is complete despite no prior events
        let stats = c.stats.read().unwrap();
        assert!(stats.roots_complete.contains_key("api"));
        // has_activity is still false (roots_queued == 0)
        assert!(
            !stats.has_activity(),
            "roots_queued not incremented — no RootDiscovered fired"
        );
    }

    /// EnrichmentComplete with server_name populates lsp_stats.
    #[tokio::test]
    async fn test_enrichment_complete_records_lsp_stats() {
        let c = make_consumer();
        // Start enrichment (records start time)
        c.on_event(&ExtractionEvent::LanguageDetected {
            slug: slug("api"),
            language: "rust".into(),
            nodes: empty_arc_nodes(),
        })
        .await
        .unwrap();
        // Complete enrichment with server_name
        c.on_event(&ExtractionEvent::EnrichmentComplete {
            slug: slug("api"),
            language: "rust".into(),
            added_edges: empty_arc_edges(),
            new_nodes: empty_arc_nodes(),
            updated_nodes: std::sync::Arc::from([]),
            server_name: Some("rust-analyzer".to_string()),
            error_count: 5,
            server_missing: false,
            remediation: Some("fix rust".to_string()),
            aborted: true,
            diagnostic: Some("forced abort".to_string()),
            validation: Some(LspValidationEvidence::not_validated(
                "rust",
                "rust-analyzer",
                "forced abort",
            )),
        })
        .await
        .unwrap();

        let stats = c.stats.read().unwrap();
        let lsp = stats
            .lsp_stats
            .get("api")
            .expect("api should have lsp_stats")
            .get("rust")
            .expect("rust should have LspLanguageStats");
        assert_eq!(lsp.server_name, "rust-analyzer");
        assert_eq!(lsp.edge_count, 0);
        assert_eq!(lsp.node_count, 0);
        assert_eq!(lsp.error_count, 5);
        assert!(lsp.aborted);
        let validation = lsp
            .validation
            .as_ref()
            .expect("not-validated evidence should be delivered");
        assert_eq!(validation.status, LspValidationStatus::NotValidated);
        assert_eq!(validation.symbol_count, None);
        // Duration is non-zero because LanguageDetected fires before EnrichmentComplete.
        // (In practice the test runs so fast this may be 0, so just assert it parsed.)
        let _ = lsp.duration;
    }

    #[tokio::test]
    async fn test_lsp_query_metrics_are_delivered_to_language_stats() {
        let c = make_consumer();
        c.on_event(&ExtractionEvent::EnrichmentComplete {
            slug: slug("api"),
            language: "rust".into(),
            added_edges: empty_arc_edges(),
            new_nodes: empty_arc_nodes(),
            updated_nodes: std::sync::Arc::from([]),
            server_name: Some("rust-analyzer".to_string()),
            error_count: 0,
            server_missing: false,
            remediation: None,
            aborted: false,
            diagnostic: None,
            validation: None,
        })
        .await
        .unwrap();
        c.on_event(&ExtractionEvent::LspQueryMetrics {
            slug: slug("api"),
            language: "rust".into(),
            metrics: std::sync::Arc::from(
                vec![crate::extract::lsp::LspQueryMetric {
                    language: "rust".to_string(),
                    server: "rust-analyzer".to_string(),
                    operation: "call_hierarchy".to_string(),
                    declaration_class: "function".to_string(),
                    scheduled_requests: 3,
                    non_empty_responses: 2,
                    emitted_edges: 4,
                    latency_ms: 17,
                    timeouts: 0,
                    errors: 0,
                }]
                .into_boxed_slice(),
            ),
        })
        .await
        .unwrap();

        let stats = c.stats.read().unwrap();
        let metrics = &stats.lsp_stats["api"]["rust"].query_metrics;
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].scheduled_requests, 3);
        assert_eq!(metrics[0].emitted_edges, 4);
    }

    /// EnrichmentComplete without server_name does NOT populate lsp_stats.
    #[tokio::test]
    async fn test_enrichment_complete_without_server_name_no_lsp_stats() {
        let c = make_consumer();
        c.on_event(&ExtractionEvent::EnrichmentComplete {
            slug: slug("api"),
            language: "rust".into(),
            added_edges: empty_arc_edges(),
            new_nodes: empty_arc_nodes(),
            updated_nodes: std::sync::Arc::from([]),
            server_name: None,
            error_count: 0,
            server_missing: false,
            remediation: None,
            aborted: false,
            diagnostic: None,
            validation: None,
        })
        .await
        .unwrap();

        let stats = c.stats.read().unwrap();
        assert!(
            stats.lsp_stats.get("api").is_none(),
            "no lsp_stats when server_name is None"
        );
    }

    #[test]
    fn test_lsp_entry_summary_ok() {
        let e = LspEnrichmentEntry {
            language: "rust".into(),
            server_name: "rust-analyzer".into(),
            root_hint: None,
            edge_count: 150,
            node_count: 42,
            error_count: 0,
            duration: Duration::from_secs_f64(3.5),
            status: LspStatus::Ok,
            remediation: None,
            query_metrics: Vec::new(),
            validation: Some(LspValidationEvidence::processed(
                "rust",
                "rust-analyzer",
                "textDocument/documentSymbol",
                0,
            )),
        };
        let l = e.summary_line();
        assert!(l.contains("rust (rust-analyzer)"));
        assert!(l.contains("OK"));
        assert!(l.contains("150 edges"));
        assert!(l.contains("processed via textDocument/documentSymbol (0 symbols)"));
    }

    #[test]
    fn test_quiescent_validation_has_no_symbol_count() {
        let evidence =
            LspValidationEvidence::quiescent("rust", "rust-analyzer", "experimental/serverStatus");

        assert_eq!(evidence.status, LspValidationStatus::Quiescent);
        assert_eq!(evidence.symbol_count, None);
        assert_eq!(
            evidence.summary(),
            "quiescent via experimental/serverStatus"
        );
    }

    #[test]
    fn test_lsp_entry_summary_not_found() {
        let e = LspEnrichmentEntry {
            language: "json".into(),
            server_name: "vscode-json-ls".into(),
            root_hint: None,
            edge_count: 0,
            node_count: 0,
            error_count: 0,
            duration: Duration::from_millis(5),
            status: LspStatus::NotFound,
            remediation: Some("install json ls".to_string()),
            query_metrics: Vec::new(),
            validation: None,
        };
        let line = e.summary_line();
        assert!(line.contains("not found"));
        assert!(line.contains("install json ls"));
    }
}
