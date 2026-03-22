//! Consumers of `LanguageDetected` events.
//!
//! These run after extraction accumulates nodes by language. Each LSP enricher
//! starts its language server once per language and enriches all matching nodes.
//!
//! Multiple LSP enrichers can run concurrently (one per language) — see the
//! ADR at `docs/ADRs/001-event-bus-extraction-pipeline.md`.
pub mod lsp;
