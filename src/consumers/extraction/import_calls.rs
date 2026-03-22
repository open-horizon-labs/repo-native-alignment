//! Import calls consumer — subscribes to RootExtracted.
//!
//! Connects import declarations to the symbols they import via Calls edges,
//! enabling cross-file call graph traversal.
//!
//! This module re-exports from `crate::extract::import_calls`. It exists here to
//! make the detection hierarchy visible: consumers triggered by RootExtracted
//! live in `consumers/extraction/`.
pub use crate::extract::import_calls::*;
