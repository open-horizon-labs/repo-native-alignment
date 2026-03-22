//! WebSocket consumer — subscribes to FrameworkDetected("socketio").
//!
//! Detects Socket.IO event patterns and emits Produces/Consumes edges
//! between event emitters and event handlers.
//!
//! This module re-exports from `crate::extract::websocket`. It exists here to
//! make the detection hierarchy visible: consumers triggered by FrameworkDetected
//! live in `consumers/framework/`.
pub use crate::extract::websocket::*;
