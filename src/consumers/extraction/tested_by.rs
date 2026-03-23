//! Tested-by consumer — subscribes to `AllEnrichmentsDone`.
//!
//! Detects test functions that follow naming conventions (test_foo tests foo)
//! and emits TestedBy edges between test nodes and the functions they test.
//!
//! This module re-exports `TestedByConsumer` from `crate::extract::consumers`. It exists
//! here to make the detection hierarchy visible: consumers triggered by `AllEnrichmentsDone`
//! live in `consumers/extraction/`.
pub use crate::extract::consumers::TestedByConsumer;
