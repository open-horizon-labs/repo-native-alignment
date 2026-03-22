//! Directory module consumer — subscribes to RootExtracted.
//!
//! Emits BelongsTo edges from every node to a virtual Module node derived
//! from its directory path, enabling directory-level graph traversal.
//!
//! This module re-exports from `crate::extract::directory_module`. It exists here to
//! make the detection hierarchy visible: consumers triggered by RootExtracted
//! live in `consumers/extraction/`.
pub use crate::extract::directory_module::*;
