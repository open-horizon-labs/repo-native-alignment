//! Framework detection consumer — subscribes to RootExtracted, emits FrameworkDetected.
//!
//! Scans Import nodes against a lookup table of import patterns, detects
//! frameworks (kafka, nextjs-app-router, socketio, etc.), and emits
//! NodeKind::Other("framework") nodes.
//!
//! This module re-exports from `crate::extract::framework_detection`. It exists
//! here to make the detection hierarchy visible: the framework detection consumer
//! triggers on RootExtracted and itself emits FrameworkDetected events, which
//! consumers in `consumers/framework/` then subscribe to.
pub use crate::extract::framework_detection::*;
