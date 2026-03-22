//! LSP consumer — subscribes to LanguageDetected.
//!
//! Spawns language servers as child processes and uses them to discover
//! cross-file edges (calls, implementations, references) that tree-sitter
//! cannot see.
//!
//! This module re-exports from `crate::extract::lsp`. It exists here to make
//! the detection hierarchy visible: LSP enrichers are triggered by
//! LanguageDetected events and live in `consumers/language/`.
pub use crate::extract::lsp::*;
