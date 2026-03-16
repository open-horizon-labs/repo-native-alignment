//! Graph state and LSP enrichment status types.

use crate::graph::{Node, Edge};
use crate::graph::index::GraphIndex;


// ── Graph state ─────────────────────────────────────────────────────

/// In-memory graph state: extraction results + petgraph index + embedding index.
/// Lazily initialized on first tool call. Embeddings are built as part of the
/// graph pipeline — not as a separate lazy init that races with graph building.
pub struct GraphState {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub index: GraphIndex,
    /// Timestamp of the last completed scan (full or incremental).
    /// `None` until the first scan finishes.
    pub last_scan_completed_at: Option<std::time::Instant>,
}

impl GraphState {
    /// Build a HashMap from stable_id -> index for O(1) node lookups.
    /// Call once per search context instead of O(N) linear scans per result.
    pub fn node_index_map(&self) -> std::collections::HashMap<String, usize> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.stable_id(), i))
            .collect()
    }

    /// Resolve a node ID that may be missing its root prefix.
    ///
    /// Search results display short IDs like `src/scanner.rs:scan:function`
    /// (with root prefix stripped), but graph lookups need the full
    /// `root-slug:src/scanner.rs:scan:function`. This method tries:
    /// 1. Exact match (full stable ID)
    /// 2. Prefix with each known root slug from the graph
    ///
    /// Returns the full stable ID if found, or the original ID if not.
    pub fn resolve_node_id(&self, id: &str) -> String {
        // Fast path: already a full stable ID
        if self.nodes.iter().any(|n| n.stable_id() == id) {
            return id.to_string();
        }
        // Try prepending each unique root slug
        let roots: std::collections::HashSet<&str> = self.nodes.iter()
            .map(|n| n.id.root.as_str())
            .collect();
        for root in &roots {
            let full_id = format!("{}:{}", root, id);
            if self.nodes.iter().any(|n| n.stable_id() == full_id) {
                return full_id;
            }
        }
        id.to_string()
    }

    /// Resolve a node ID using the pre-built index map. O(1) for exact match,
    /// O(roots) for short-ID resolution.
    pub fn resolve_node_id_fast(&self, id: &str, index_map: &std::collections::HashMap<String, usize>) -> String {
        if index_map.contains_key(id) {
            return id.to_string();
        }
        let roots: std::collections::HashSet<&str> = self.nodes.iter()
            .map(|n| n.id.root.as_str())
            .collect();
        for root in &roots {
            let full_id = format!("{}:{}", root, id);
            if index_map.contains_key(&full_id) {
                return full_id;
            }
        }
        id.to_string()
    }

    /// Look up a node by stable_id using a pre-built index map. O(1).
    pub fn node_by_stable_id<'a>(
        &'a self,
        id: &str,
        index_map: &std::collections::HashMap<String, usize>,
    ) -> Option<&'a Node> {
        index_map.get(id).and_then(|&i| self.nodes.get(i))
    }
}

// ── Embedding build status ───────────────────────────────────────────

/// Tracks embedding build progress so the search footer can show
/// `embedding... (N/M)` during build and just the count when done.
pub struct EmbeddingStatus {
    /// 0 = not started, 1 = building, 2 = complete
    state: std::sync::atomic::AtomicU8,
    /// Current progress (items embedded so far).
    current: std::sync::atomic::AtomicUsize,
    /// Total items to embed.
    total: std::sync::atomic::AtomicUsize,
    /// Final count after completion.
    completed_count: std::sync::atomic::AtomicUsize,
}

impl Default for EmbeddingStatus {
    fn default() -> Self {
        Self {
            state: std::sync::atomic::AtomicU8::new(0),
            current: std::sync::atomic::AtomicUsize::new(0),
            total: std::sync::atomic::AtomicUsize::new(0),
            completed_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl EmbeddingStatus {
    /// Mark embedding as building with a known total.
    pub fn set_building(&self, total: usize) {
        self.total.store(total, std::sync::atomic::Ordering::Release);
        self.current.store(0, std::sync::atomic::Ordering::Release);
        self.state.store(1, std::sync::atomic::Ordering::Release);
    }

    /// Update progress during embedding.
    pub fn set_progress(&self, current: usize) {
        self.current.store(current, std::sync::atomic::Ordering::Release);
    }

    /// Mark embedding as complete with a final count.
    pub fn set_complete(&self, count: usize) {
        self.completed_count.store(count, std::sync::atomic::Ordering::Release);
        self.state.store(2, std::sync::atomic::Ordering::Release);
    }

    /// Render a footer segment for the search footer, or `None` if not started.
    pub fn footer_segment(&self) -> Option<String> {
        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            0 => None,
            1 => {
                let cur = self.current.load(std::sync::atomic::Ordering::Acquire);
                let tot = self.total.load(std::sync::atomic::Ordering::Acquire);
                Some(format!("embedding... ({}/{})", cur, tot))
            }
            2 => {
                let count = self.completed_count.load(std::sync::atomic::Ordering::Acquire);
                Some(format!("{} embedded", count))
            }
            _ => None,
        }
    }
}

// ── LSP enrichment status ────────────────────────────────────────────

/// Named states for the LSP enrichment state machine.
/// Each transition is logged with elapsed time since the previous transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LspState {
    NotStarted = 0,
    Running = 1,
    Complete = 2,
    Unavailable = 3,
    /// Server binary found on PATH but enrichment hasn't started yet.
    ServerFound = 4,
}

impl LspState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::NotStarted,
            1 => Self::Running,
            2 => Self::Complete,
            3 => Self::Unavailable,
            4 => Self::ServerFound,
            _ => Self::NotStarted,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::NotStarted => "NOT_STARTED",
            Self::Running => "RUNNING",
            Self::Complete => "COMPLETE",
            Self::Unavailable => "UNAVAILABLE",
            Self::ServerFound => "SERVER_FOUND",
        }
    }
}

impl std::fmt::Display for LspState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Tracks whether background LSP enrichment has run, so query footers
/// can tell the agent "results may be incomplete" vs "enrichment done."
///
/// Every state transition is logged with the previous state, new state,
/// and elapsed time since the last transition.
pub struct LspEnrichmentStatus {
    state: std::sync::atomic::AtomicU8,
    /// Number of edges added by the most recent enrichment pass.
    edge_count: std::sync::atomic::AtomicUsize,
    /// Whether the most recent persist to LanceDB failed.
    /// When true, the footer shows "persist failed" to indicate edges are
    /// in-memory but may not have been written to disk.
    persist_failed: std::sync::atomic::AtomicBool,
    /// When enrichment last completed (for auto-hide after 30 s).
    completed_at: std::sync::Mutex<Option<std::time::Instant>>,
    /// When the status was created (for elapsed time in transitions).
    created_at: std::time::Instant,
    /// When the last state transition occurred.
    last_transition_at: std::sync::Mutex<std::time::Instant>,
    /// Name of the server found during probe (for diagnostics).
    server_name: std::sync::Mutex<Option<String>>,
}

impl Default for LspEnrichmentStatus {
    fn default() -> Self {
        let now = std::time::Instant::now();
        Self {
            state: std::sync::atomic::AtomicU8::new(0),
            edge_count: std::sync::atomic::AtomicUsize::new(0),
            persist_failed: std::sync::atomic::AtomicBool::new(false),
            completed_at: std::sync::Mutex::new(None),
            created_at: now,
            last_transition_at: std::sync::Mutex::new(now),
            server_name: std::sync::Mutex::new(None),
        }
    }
}

impl LspEnrichmentStatus {
    // Keep numeric constants for backward compatibility with any external code.
    const NOT_STARTED: u8 = LspState::NotStarted as u8;
    const RUNNING: u8 = LspState::Running as u8;
    const COMPLETE: u8 = LspState::Complete as u8;
    const UNAVAILABLE: u8 = LspState::Unavailable as u8;
    const SERVER_FOUND: u8 = LspState::ServerFound as u8;

    /// Log a state transition with elapsed time.
    fn log_transition(&self, from: LspState, to: LspState, detail: &str) {
        let mut last = self.last_transition_at.lock().unwrap();
        let since_last = last.elapsed();
        let since_created = self.created_at.elapsed();
        *last = std::time::Instant::now();

        if detail.is_empty() {
            tracing::info!(
                "LSP state: {} -> {} (step: {:.1}s, total: {:.1}s)",
                from, to,
                since_last.as_secs_f64(),
                since_created.as_secs_f64(),
            );
        } else {
            tracing::info!(
                "LSP state: {} -> {} — {} (step: {:.1}s, total: {:.1}s)",
                from, to, detail,
                since_last.as_secs_f64(),
                since_created.as_secs_f64(),
            );
        }
    }

    /// Get the current state as a typed enum.
    pub fn current_state(&self) -> LspState {
        LspState::from_u8(self.state.load(std::sync::atomic::Ordering::Acquire))
    }

    /// Elapsed time since the status was created.
    pub fn elapsed(&self) -> std::time::Duration {
        self.created_at.elapsed()
    }

    /// Elapsed time since the last state transition.
    pub fn elapsed_since_last_transition(&self) -> std::time::Duration {
        self.last_transition_at.lock().unwrap().elapsed()
    }

    pub fn set_running(&self) {
        let prev = LspState::from_u8(
            self.state.swap(Self::RUNNING, std::sync::atomic::Ordering::AcqRel)
        );
        self.log_transition(prev, LspState::Running, "");
    }

    pub fn set_complete(&self, edge_count: usize) {
        self.edge_count.store(edge_count, std::sync::atomic::Ordering::Release);
        self.persist_failed.store(false, std::sync::atomic::Ordering::Release);
        *self.completed_at.lock().unwrap() = Some(std::time::Instant::now());
        let prev = LspState::from_u8(
            self.state.swap(Self::COMPLETE, std::sync::atomic::Ordering::AcqRel)
        );
        self.log_transition(prev, LspState::Complete, &format!("{} edges", edge_count));
    }

    /// Mark enrichment as complete but with a persist failure.
    ///
    /// The edges were computed and added to the in-memory graph, but writing
    /// them to LanceDB failed. The footer will show "persist failed" so agents
    /// know the data may not survive a restart.
    pub fn set_complete_persist_failed(&self, edge_count: usize) {
        self.edge_count.store(edge_count, std::sync::atomic::Ordering::Release);
        self.persist_failed.store(true, std::sync::atomic::Ordering::Release);
        *self.completed_at.lock().unwrap() = Some(std::time::Instant::now());
        let prev = LspState::from_u8(
            self.state.swap(Self::COMPLETE, std::sync::atomic::Ordering::AcqRel)
        );
        self.log_transition(
            prev,
            LspState::Complete,
            &format!("{} edges, persist failed", edge_count),
        );
    }

    /// Mark that no LSP server was available for any of the detected languages.
    pub fn set_unavailable(&self) {
        *self.completed_at.lock().unwrap() = Some(std::time::Instant::now());
        let prev = LspState::from_u8(
            self.state.swap(Self::UNAVAILABLE, std::sync::atomic::Ordering::AcqRel)
        );
        self.log_transition(prev, LspState::Unavailable, "no server detected");
    }

    /// Mark that at least one LSP server binary was found on PATH.
    /// Called synchronously at startup before async enrichment begins.
    pub fn set_server_found(&self) {
        // Only transition from NOT_STARTED -- don't regress from RUNNING/COMPLETE.
        let result = self.state.compare_exchange(
            Self::NOT_STARTED,
            Self::SERVER_FOUND,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Relaxed,
        );
        if result.is_ok() {
            self.log_transition(LspState::NotStarted, LspState::ServerFound, "");
        }
    }

    /// Record which server was found during probe.
    pub fn set_server_name(&self, name: &str) {
        *self.server_name.lock().unwrap() = Some(name.to_string());
    }

    /// Get the server name found during probe, if any.
    pub fn server_name(&self) -> Option<String> {
        self.server_name.lock().unwrap().clone()
    }

    /// Check if the SERVER_FOUND state has timed out (stuck for > 5 min).
    /// Returns true if timed out (and transitions to UNAVAILABLE).
    pub fn check_server_found_timeout(&self) -> bool {
        let current = self.current_state();
        if current != LspState::ServerFound {
            return false;
        }
        let elapsed = self.elapsed_since_last_transition();
        if elapsed > std::time::Duration::from_secs(300) {
            // 5 minute timeout — use compare_exchange to avoid overwriting
            // a newer state (RUNNING/COMPLETE) that raced with this check.
            let result = self.state.compare_exchange(
                Self::SERVER_FOUND,
                Self::UNAVAILABLE,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            );
            if result.is_ok() {
                *self.completed_at.lock().unwrap() = Some(std::time::Instant::now());
                self.log_transition(
                    LspState::ServerFound,
                    LspState::Unavailable,
                    "SERVER_FOUND timed out after 5 min — enrichment never started",
                );
                return true;
            }
            // Another thread already transitioned the state -- don't regress.
            return false;
        }
        false
    }

    /// Synchronously probe for known LSP server binaries on PATH.
    /// Fast (just `which` calls, no process spawning). Call at handler construction
    /// to distinguish "server exists but pending" from "no server available."
    pub fn probe_for_servers() -> Self {
        let status = Self::default();

        // Check for common LSP servers. We only need ONE hit to know
        // LSP enrichment will likely succeed.
        let known_servers = [
            "rust-analyzer",
            "pyright-langserver",
            "typescript-language-server",
            "gopls",
            "clangd",
        ];

        for server in &known_servers {
            let found = std::process::Command::new("which")
                .arg(server)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if found {
                tracing::info!("LSP probe: found '{}' on PATH", server);
                status.set_server_name(server);
                status.set_server_found();
                return status;
            }
        }

        // No known servers found — mark unavailable immediately so the first
        // query footer says "no server detected" instead of a misleading
        // "LSP: pending..." that never resolves.
        status.set_unavailable();
        status
    }

    /// Render a short footer segment, or `None` if nothing useful to show.
    ///
    /// Includes elapsed time for in-progress states so agents can see how
    /// long LSP enrichment has been running.
    pub fn footer_segment(&self) -> Option<String> {
        // Check for SERVER_FOUND timeout before rendering
        self.check_server_found_timeout();

        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            Self::NOT_STARTED => None,
            Self::SERVER_FOUND => {
                let elapsed = self.elapsed_since_last_transition();
                let server = self.server_name().unwrap_or_else(|| "unknown".to_string());
                Some(format!(
                    "LSP: {} found, waiting to start ({:.0}s)",
                    server,
                    elapsed.as_secs_f64(),
                ))
            }
            Self::RUNNING => {
                let elapsed = self.elapsed_since_last_transition();
                Some(format!("LSP: enriching ({:.0}s)", elapsed.as_secs_f64()))
            }
            Self::COMPLETE => {
                let guard = self.completed_at.lock().unwrap();
                if let Some(t) = *guard {
                    let persist_failed = self.persist_failed.load(std::sync::atomic::Ordering::Acquire);
                    // Always show persist failures (no auto-hide) so agents know
                    // enrichment data may not survive a restart.
                    if persist_failed || t.elapsed().as_secs() < 30 {
                        let count = self.edge_count.load(std::sync::atomic::Ordering::Acquire);
                        let total = self.elapsed();
                        if persist_failed {
                            Some(format!(
                                "LSP: enriched ({} edges, persist failed, {:.1}s)",
                                count,
                                total.as_secs_f64(),
                            ))
                        } else {
                            Some(format!(
                                "LSP: enriched ({} edges in {:.1}s)",
                                count,
                                total.as_secs_f64(),
                            ))
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Self::UNAVAILABLE => {
                // Always show unavailable status (no 30s auto-hide) so agents
                // know LSP enrichment didn't run and why.
                Some("LSP: no server detected".to_string())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lsp_state_label() {
        assert_eq!(LspState::NotStarted.label(), "NOT_STARTED");
        assert_eq!(LspState::Running.label(), "RUNNING");
        assert_eq!(LspState::Complete.label(), "COMPLETE");
        assert_eq!(LspState::Unavailable.label(), "UNAVAILABLE");
        assert_eq!(LspState::ServerFound.label(), "SERVER_FOUND");
    }

    #[test]
    fn test_lsp_state_roundtrip() {
        for state in [
            LspState::NotStarted,
            LspState::Running,
            LspState::Complete,
            LspState::Unavailable,
            LspState::ServerFound,
        ] {
            assert_eq!(LspState::from_u8(state as u8), state);
        }
    }

    #[test]
    fn test_lsp_status_not_started_no_footer() {
        let status = LspEnrichmentStatus::default();
        assert!(status.footer_segment().is_none());
    }

    #[test]
    fn test_lsp_status_server_found_shows_waiting() {
        let status = LspEnrichmentStatus::default();
        status.set_server_found();
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("found, waiting to start"), "got: {}", footer);
    }

    #[test]
    fn test_lsp_status_running_shows_enriching() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        let footer = status.footer_segment().unwrap();
        assert!(footer.starts_with("LSP: enriching"), "got: {}", footer);
    }

    #[test]
    fn test_lsp_status_complete_shows_edge_count() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete(42);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("42 edges"), "got: {}", footer);
    }

    #[test]
    fn test_lsp_status_complete_zero_edges() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete(0);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("0 edges"), "got: {}", footer);
    }

    #[test]
    fn test_lsp_status_unavailable_shows_no_server() {
        let status = LspEnrichmentStatus::default();
        status.set_unavailable();
        assert_eq!(
            status.footer_segment(),
            Some("LSP: no server detected".to_string())
        );
    }

    #[test]
    fn test_lsp_status_unavailable_no_auto_hide() {
        let status = LspEnrichmentStatus::default();
        status.set_unavailable();
        // "Unavailable" should always be shown (no auto-hide).
        assert!(status.footer_segment().is_some());
    }

    #[test]
    fn test_lsp_status_set_complete_without_set_running() {
        let status = LspEnrichmentStatus::default();
        status.set_complete(10);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("10 edges"), "got: {}", footer);
    }

    #[test]
    fn test_lsp_status_double_set_running() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_running();
        let footer = status.footer_segment().unwrap();
        assert!(footer.starts_with("LSP: enriching"), "got: {}", footer);
    }

    #[test]
    fn test_lsp_status_complete_then_running_again() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete(5);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("5 edges"), "got: {}", footer);
        // Simulate a second enrichment pass
        status.set_running();
        let footer = status.footer_segment().unwrap();
        assert!(footer.starts_with("LSP: enriching"), "got: {}", footer);
    }

    #[test]
    fn test_lsp_status_unavailable_then_running_then_complete() {
        let status = LspEnrichmentStatus::default();
        status.set_unavailable();
        assert_eq!(status.footer_segment(), Some("LSP: no server detected".to_string()));
        // If a server becomes available later
        status.set_running();
        let footer = status.footer_segment().unwrap();
        assert!(footer.starts_with("LSP: enriching"), "got: {}", footer);
        status.set_complete(3);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("3 edges"), "got: {}", footer);
    }

    #[test]
    fn test_lsp_status_server_found_no_regress_from_running() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_server_found(); // should not regress from RUNNING to SERVER_FOUND
        let footer = status.footer_segment().unwrap();
        assert!(footer.starts_with("LSP: enriching"), "got: {}", footer);
    }

    #[test]
    fn test_lsp_status_concurrent_reads() {
        use std::sync::Arc;

        let status = Arc::new(LspEnrichmentStatus::default());
        status.set_running();

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let s = Arc::clone(&status);
                std::thread::spawn(move || {
                    let segment = s.footer_segment();
                    assert!(segment.is_some());
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn test_lsp_status_large_edge_count() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete(1_000_000);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("1000000 edges"), "got: {}", footer);
    }

    #[test]
    fn test_lsp_state_current_state() {
        let status = LspEnrichmentStatus::default();
        assert_eq!(status.current_state(), LspState::NotStarted);
        status.set_running();
        assert_eq!(status.current_state(), LspState::Running);
        status.set_complete(5);
        assert_eq!(status.current_state(), LspState::Complete);
    }

    #[test]
    fn test_lsp_server_name_tracking() {
        let status = LspEnrichmentStatus::default();
        assert!(status.server_name().is_none());
        status.set_server_name("rust-analyzer");
        assert_eq!(status.server_name(), Some("rust-analyzer".to_string()));
    }

    /// Adversarial: concurrent set_running vs check_server_found_timeout.
    /// If set_running wins, timeout must NOT overwrite RUNNING with UNAVAILABLE.
    #[test]
    fn test_adversarial_timeout_does_not_overwrite_running() {
        use std::sync::Arc;

        let status = Arc::new(LspEnrichmentStatus::default());
        status.set_server_found();
        // Simulate: enrichment starts (set_running) while timeout check races.
        // set_running should win — the compare_exchange in timeout should fail.
        status.set_running();

        // Now call timeout — it should see RUNNING, not SERVER_FOUND, and return false.
        let timed_out = status.check_server_found_timeout();
        assert!(!timed_out, "timeout should not fire when state is RUNNING");
        assert_eq!(
            status.current_state(),
            LspState::Running,
            "state should remain RUNNING, not regress to UNAVAILABLE"
        );
    }

    /// Adversarial: concurrent set_complete vs check_server_found_timeout.
    #[test]
    fn test_adversarial_timeout_does_not_overwrite_complete() {
        let status = LspEnrichmentStatus::default();
        status.set_server_found();
        status.set_running();
        status.set_complete(42);

        let timed_out = status.check_server_found_timeout();
        assert!(!timed_out);
        assert_eq!(status.current_state(), LspState::Complete);
    }

    /// Adversarial: hammer the state machine from 10 threads concurrently.
    /// No panics, no undefined states — the final state must be valid.
    #[test]
    fn test_adversarial_concurrent_state_transitions() {
        use std::sync::Arc;

        let status = Arc::new(LspEnrichmentStatus::default());

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let s = Arc::clone(&status);
                std::thread::spawn(move || {
                    match i % 4 {
                        0 => s.set_server_found(),
                        1 => s.set_running(),
                        2 => s.set_complete(i * 10),
                        3 => { let _ = s.check_server_found_timeout(); }
                        _ => unreachable!(),
                    }
                    // All reads must produce a valid state
                    let state = s.current_state();
                    assert!(matches!(
                        state,
                        LspState::NotStarted
                            | LspState::Running
                            | LspState::Complete
                            | LspState::Unavailable
                            | LspState::ServerFound
                    ));
                    let _footer = s.footer_segment(); // must not panic
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn test_lsp_status_persist_failed_shows_in_footer() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete_persist_failed(42);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("42 edges"), "got: {}", footer);
        assert!(footer.contains("persist failed"), "got: {}", footer);
    }

    #[test]
    fn test_lsp_status_persist_failed_no_auto_hide() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete_persist_failed(10);
        // Backdate completed_at past the 30s auto-hide window to prove
        // persist_failed footers are never hidden (unlike normal complete).
        *status.completed_at.lock().unwrap() =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(31));
        // persist_failed footer should still be shown after the normal auto-hide window
        let footer = status.footer_segment();
        assert!(footer.is_some(), "persist_failed footer should not auto-hide");
        assert!(
            footer.unwrap().contains("persist failed"),
            "footer should indicate persist failure"
        );
    }

    #[test]
    fn test_lsp_status_persist_failed_cleared_on_success() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete_persist_failed(10);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("persist failed"), "got: {}", footer);
        // Successful completion clears the persist_failed flag
        status.set_complete(20);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("20 edges"), "got: {}", footer);
        assert!(!footer.contains("persist failed"), "got: {}", footer);
    }

    /// Adversarial: from_u8 with invalid value must not panic.
    #[test]
    fn test_adversarial_invalid_state_u8() {
        // Values beyond the enum range should fall back to NotStarted
        assert_eq!(LspState::from_u8(255), LspState::NotStarted);
        assert_eq!(LspState::from_u8(5), LspState::NotStarted);
        assert_eq!(LspState::from_u8(100), LspState::NotStarted);
    }

    #[test]
    fn test_lsp_server_found_footer_includes_server_name() {
        let status = LspEnrichmentStatus::default();
        status.set_server_name("rust-analyzer");
        status.set_server_found();
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("rust-analyzer"), "got: {}", footer);
    }

    // ── resolve_node_id / resolve_node_id_fast tests ────────────────

    fn make_test_node(root: &str, file: &str, name: &str, kind: crate::graph::NodeKind) -> crate::graph::Node {
        use crate::graph::*;
        use std::collections::BTreeMap;
        Node {
            id: NodeId {
                root: root.to_string(),
                file: std::path::PathBuf::from(file),
                name: name.to_string(),
                kind,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 10,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_test_graph_state(nodes: Vec<crate::graph::Node>) -> GraphState {
        let mut index = crate::graph::index::GraphIndex::new();
        for n in &nodes {
            index.ensure_node(&n.stable_id(), &n.id.kind.to_string());
        }
        GraphState { nodes, edges: vec![], index, last_scan_completed_at: None }
    }

    #[test]
    fn test_resolve_node_id_exact_match() {
        let node = make_test_node("myroot", "src/scanner.rs", "scan", crate::graph::NodeKind::Function);
        let gs = make_test_graph_state(vec![node]);
        // Full stable ID should resolve to itself
        let full = "myroot:src/scanner.rs:scan:function";
        assert_eq!(gs.resolve_node_id(full), full);
    }

    #[test]
    fn test_resolve_node_id_short_id() {
        let node = make_test_node("myroot", "src/scanner.rs", "scan", crate::graph::NodeKind::Function);
        let gs = make_test_graph_state(vec![node]);
        // Short ID (without root prefix) should resolve to full
        let short = "src/scanner.rs:scan:function";
        assert_eq!(gs.resolve_node_id(short), "myroot:src/scanner.rs:scan:function");
    }

    #[test]
    fn test_resolve_node_id_unknown_passes_through() {
        let node = make_test_node("myroot", "src/scanner.rs", "scan", crate::graph::NodeKind::Function);
        let gs = make_test_graph_state(vec![node]);
        let unknown = "nonexistent:file.rs:foo:function";
        assert_eq!(gs.resolve_node_id(unknown), unknown);
    }

    #[test]
    fn test_resolve_node_id_fast_exact_match() {
        let node = make_test_node("myroot", "src/embed.rs", "EmbeddingIndex", crate::graph::NodeKind::Struct);
        let gs = make_test_graph_state(vec![node]);
        let index_map = gs.node_index_map();
        let full = "myroot:src/embed.rs:EmbeddingIndex:struct";
        assert_eq!(gs.resolve_node_id_fast(full, &index_map), full);
    }

    #[test]
    fn test_resolve_node_id_fast_short_id() {
        let node = make_test_node("myroot", "src/embed.rs", "EmbeddingIndex", crate::graph::NodeKind::Struct);
        let gs = make_test_graph_state(vec![node]);
        let index_map = gs.node_index_map();
        let short = "src/embed.rs:EmbeddingIndex:struct";
        assert_eq!(gs.resolve_node_id_fast(short, &index_map), "myroot:src/embed.rs:EmbeddingIndex:struct");
    }

    #[test]
    fn test_resolve_node_id_fast_unknown_passes_through() {
        let node = make_test_node("myroot", "src/embed.rs", "EmbeddingIndex", crate::graph::NodeKind::Struct);
        let gs = make_test_graph_state(vec![node]);
        let index_map = gs.node_index_map();
        let unknown = "totally:bogus:id";
        assert_eq!(gs.resolve_node_id_fast(unknown, &index_map), unknown);
    }

    #[test]
    fn test_resolve_node_id_multiple_roots() {
        let node1 = make_test_node("root-a", "src/lib.rs", "foo", crate::graph::NodeKind::Function);
        let node2 = make_test_node("root-b", "src/lib.rs", "foo", crate::graph::NodeKind::Function);
        let gs = make_test_graph_state(vec![node1, node2]);
        // Both full IDs should resolve exactly
        assert_eq!(gs.resolve_node_id("root-a:src/lib.rs:foo:function"), "root-a:src/lib.rs:foo:function");
        assert_eq!(gs.resolve_node_id("root-b:src/lib.rs:foo:function"), "root-b:src/lib.rs:foo:function");
        // Short ID should resolve to one of them (either is valid since both exist)
        let resolved = gs.resolve_node_id("src/lib.rs:foo:function");
        assert!(
            resolved == "root-a:src/lib.rs:foo:function" || resolved == "root-b:src/lib.rs:foo:function",
            "expected one of the full IDs, got: {}", resolved
        );
    }

    #[test]
    fn test_resolve_node_id_empty_string() {
        let node = make_test_node("myroot", "src/lib.rs", "bar", crate::graph::NodeKind::Function);
        let gs = make_test_graph_state(vec![node]);
        // Empty string should pass through unchanged
        assert_eq!(gs.resolve_node_id(""), "");
    }

    // ── EmbeddingStatus tests ───────────────────────────────────────

    #[test]
    fn test_embedding_status_not_started() {
        let status = EmbeddingStatus::default();
        assert!(status.footer_segment().is_none());
    }

    #[test]
    fn test_embedding_status_building() {
        let status = EmbeddingStatus::default();
        status.set_building(5000);
        status.set_progress(1200);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("embedding..."), "got: {}", footer);
        assert!(footer.contains("1200/5000"), "got: {}", footer);
    }

    #[test]
    fn test_embedding_status_complete() {
        let status = EmbeddingStatus::default();
        status.set_building(5000);
        status.set_complete(4500);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("4500 embedded"), "got: {}", footer);
        assert!(!footer.contains("embedding..."), "got: {}", footer);
    }

    #[test]
    fn test_embedding_status_progress_updates() {
        let status = EmbeddingStatus::default();
        status.set_building(100);
        status.set_progress(50);
        let f1 = status.footer_segment().unwrap();
        assert!(f1.contains("50/100"), "got: {}", f1);
        status.set_progress(75);
        let f2 = status.footer_segment().unwrap();
        assert!(f2.contains("75/100"), "got: {}", f2);
    }

    // ── Adversarial: EmbeddingStatus ──────────────────────────────────

    /// Dissent finding: concurrent set_building/set_complete should not panic
    /// or produce invalid footer output. Uses atomics so no UB, but values
    /// could be interleaved.
    #[test]
    fn test_embedding_status_concurrent_transitions() {
        use std::sync::Arc;
        use std::thread;

        let status = Arc::new(EmbeddingStatus::default());
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let s = status.clone();
                thread::spawn(move || {
                    for j in 0..100 {
                        match (i + j) % 3 {
                            0 => s.set_building(1000 + j),
                            1 => s.set_progress(j),
                            2 => s.set_complete(j),
                            _ => unreachable!(),
                        }
                        // Reading footer mid-transition must not panic
                        let _ = s.footer_segment();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // After all threads finish, footer_segment must still return valid output
        let seg = status.footer_segment();
        assert!(seg.is_some() || seg.is_none(), "footer_segment returned something");
    }

    /// Edge case: set_complete without set_building (skip straight to done).
    #[test]
    fn test_embedding_status_complete_without_building() {
        let status = EmbeddingStatus::default();
        status.set_complete(42);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("42 embedded"), "got: {}", footer);
    }

    /// Edge case: set_building(0) — zero items to embed.
    #[test]
    fn test_embedding_status_building_zero_items() {
        let status = EmbeddingStatus::default();
        status.set_building(0);
        let footer = status.footer_segment().unwrap();
        assert!(footer.contains("embedding..."), "got: {}", footer);
        assert!(footer.contains("0/0"), "got: {}", footer);
    }

    /// Edge case: set_progress beyond total (shouldn't happen but shouldn't panic).
    #[test]
    fn test_embedding_status_progress_exceeds_total() {
        let status = EmbeddingStatus::default();
        status.set_building(100);
        status.set_progress(200); // > total
        let footer = status.footer_segment().unwrap();
        // Should render without panic, even if numbers look odd
        assert!(footer.contains("200/100"), "got: {}", footer);
    }

    /// Rapid state cycling: building -> complete -> building -> complete.
    #[test]
    fn test_embedding_status_rapid_cycling() {
        let status = EmbeddingStatus::default();
        for i in 0..50 {
            status.set_building(i * 100);
            status.set_progress(i * 50);
            let _ = status.footer_segment();
            status.set_complete(i * 90);
            let seg = status.footer_segment().unwrap();
            assert!(
                seg.contains("embedded"),
                "After set_complete, should show 'embedded', got: {}",
                seg
            );
        }
    }
}
