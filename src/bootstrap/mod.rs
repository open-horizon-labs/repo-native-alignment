//! Bootstrap — emits RootDiscovered events to start the extraction pipeline.
//!
//! # Current state
//!
//! This module is a placeholder for the bootstrap function described in
//! `docs/ADRs/001-event-bus-extraction-pipeline.md`. The full implementation
//! will replace the current `workspace.resolved_roots()` call in `server/mod.rs`.
//!
//! # Design
//!
//! ```rust,ignore
//! fn bootstrap(workspace: &WorkspaceConfig) -> impl Stream<Item = ExtractionEvent> {
//!     workspace.resolved_roots()
//!         .into_iter()
//!         .map(|root| ExtractionEvent::RootDiscovered {
//!             slug: root.slug,
//!             path: root.path,
//!             lsp_only: root.config.lsp_only,
//!         })
//! }
//! ```
//!
//! One function. No knowledge of languages, extractors, LSP, or passes.
