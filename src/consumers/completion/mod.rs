//! Consumers of `PassesComplete` events.
//!
//! These run after all post-extraction passes have completed. They finalize
//! the graph with higher-level structural analysis.
pub mod subsystem;
