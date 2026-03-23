//! API link consumer — subscribes to `AllEnrichmentsDone`.
//!
//! Links HTTP handler nodes to route definitions by matching path strings
//! and method annotations across the graph.
//!
//! This module re-exports `ApiLinkConsumer` from `crate::extract::consumers`. It exists
//! here to make the detection hierarchy visible: consumers triggered by `AllEnrichmentsDone`
//! live in `consumers/extraction/`.
pub use crate::extract::consumers::ApiLinkConsumer;
