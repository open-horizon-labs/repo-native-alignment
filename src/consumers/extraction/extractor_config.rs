//! Extractor config consumer — subscribes to RootExtracted.
//!
//! Loads `.oh/extractors/*.toml` files and runs config-driven boundary
//! detection passes that emit Produces/Consumes edges.
//!
//! This module re-exports from `crate::extract::extractor_config`. It exists here to
//! make the detection hierarchy visible: consumers triggered by RootExtracted
//! live in `consumers/extraction/`.
pub use crate::extract::extractor_config::*;
