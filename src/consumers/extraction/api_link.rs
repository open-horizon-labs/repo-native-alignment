//! API link consumer — subscribes to RootExtracted.
//!
//! Links HTTP handler nodes to route definitions by matching path strings
//! and method annotations across the graph.
//!
//! This module re-exports from `crate::extract::api_link`. It exists here to
//! make the detection hierarchy visible: consumers triggered by RootExtracted
//! live in `consumers/extraction/`.
pub use crate::extract::api_link::*;
