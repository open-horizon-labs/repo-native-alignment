//! Subsystem consumer — subscribes to PassesComplete.
//!
//! Promotes subsystem clusters (detected via community detection) to
//! first-class NodeKind::Other("subsystem") nodes with BelongsTo edges
//! from member symbols.
//!
//! This module re-exports from `crate::extract::subsystem_pass`. It exists here
//! to make the detection hierarchy visible: consumers triggered by PassesComplete
//! live in `consumers/completion/`.
pub use crate::extract::subsystem_pass::*;
