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

// ── LSP enrichment status ────────────────────────────────────────────

/// Tracks whether background LSP enrichment has run, so query footers
/// can tell the agent "results may be incomplete" vs "enrichment done."
pub struct LspEnrichmentStatus {
    /// 0 = not started, 1 = running, 2 = complete
    state: std::sync::atomic::AtomicU8,
    /// Number of edges added by the most recent enrichment pass.
    edge_count: std::sync::atomic::AtomicUsize,
    /// When enrichment last completed (for auto-hide after 30 s).
    completed_at: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Default for LspEnrichmentStatus {
    fn default() -> Self {
        Self {
            state: std::sync::atomic::AtomicU8::new(0),
            edge_count: std::sync::atomic::AtomicUsize::new(0),
            completed_at: std::sync::Mutex::new(None),
        }
    }
}

impl LspEnrichmentStatus {
    /// Server binary found on PATH but enrichment hasn't started yet.
    /// (State transitions from 0→1 when set_running() is called.)
    pub fn set_running(&self) {
        self.state.store(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_complete(&self, edge_count: usize) {
        self.edge_count.store(edge_count, std::sync::atomic::Ordering::Relaxed);
        self.state.store(2, std::sync::atomic::Ordering::Relaxed);
        *self.completed_at.lock().unwrap() = Some(std::time::Instant::now());
    }

    /// Mark that no LSP server was available for any of the detected languages.
    pub fn set_unavailable(&self) {
        self.state.store(3, std::sync::atomic::Ordering::Relaxed);
    }

    /// Mark that at least one LSP server binary was found on PATH.
    /// Called synchronously at startup before async enrichment begins.
    pub fn set_server_found(&self) {
        // Transition 0→0 is a no-op; this just documents that we checked.
        // The actual transition to "running" (1) happens in set_running().
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
    pub fn footer_segment(&self) -> Option<String> {
        let state = self.state.load(std::sync::atomic::Ordering::Relaxed);
        match state {
            0 => None, // not started — nothing useful to say yet
            1 => Some("LSP: pending...".to_string()),
            2 => {
                let count = self.edge_count.load(std::sync::atomic::Ordering::Relaxed);
                // Auto-hide: suppress after 30 s so it doesn't clutter every response.
                let hide = self.completed_at.lock().unwrap()
                    .map(|t| t.elapsed().as_secs() > 30)
                    .unwrap_or(false);
                if hide {
                    None
                } else {
                    Some(format!("LSP: enriched +{} edges", count))
                }
            }
            3 => {
                // "Unavailable" is always shown — agents need to
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
    fn test_lsp_status_not_started_no_footer() {
        let status = LspEnrichmentStatus::default();
        assert!(status.footer_segment().is_none());
    }

    #[test]
    fn test_lsp_status_running_shows_pending() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        assert_eq!(status.footer_segment(), Some("LSP: pending...".to_string()));
    }

    #[test]
    fn test_lsp_status_complete_shows_edge_count() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete(42);
        assert_eq!(status.footer_segment(), Some("LSP: enriched +42 edges".to_string()));
    }

    #[test]
    fn test_lsp_status_complete_zero_edges() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete(0);
        assert_eq!(status.footer_segment(), Some("LSP: enriched +0 edges".to_string()));
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
        // We can't easily test the 30s auto-hide for complete without sleeping,
        // but we can verify that unavailable is always Some.
        assert!(status.footer_segment().is_some());
    }

    #[test]
    fn test_lsp_status_set_complete_without_set_running() {
        let status = LspEnrichmentStatus::default();
        status.set_complete(10);
        assert_eq!(status.footer_segment(), Some("LSP: enriched +10 edges".to_string()));
    }

    #[test]
    fn test_lsp_status_double_set_running() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_running();
        assert_eq!(status.footer_segment(), Some("LSP: pending...".to_string()));
    }

    #[test]
    fn test_lsp_status_complete_then_running_again() {
        let status = LspEnrichmentStatus::default();
        status.set_running();
        status.set_complete(5);
        assert_eq!(status.footer_segment(), Some("LSP: enriched +5 edges".to_string()));
        // Simulate a second enrichment pass
        status.set_running();
        assert_eq!(status.footer_segment(), Some("LSP: pending...".to_string()));
    }

    #[test]
    fn test_lsp_status_unavailable_then_running_then_complete() {
        let status = LspEnrichmentStatus::default();
        status.set_unavailable();
        assert_eq!(status.footer_segment(), Some("LSP: no server detected".to_string()));
        // If a server becomes available later
        status.set_running();
        assert_eq!(status.footer_segment(), Some("LSP: pending...".to_string()));
        status.set_complete(3);
        assert_eq!(status.footer_segment(), Some("LSP: enriched +3 edges".to_string()));
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
        assert_eq!(
            status.footer_segment(),
            Some("LSP: enriched +1000000 edges".to_string())
        );
    }
}
