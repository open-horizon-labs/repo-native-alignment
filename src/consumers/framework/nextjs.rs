//! Next.js consumer — subscribes to FrameworkDetected("nextjs-app-router").
//!
//! Turns Next.js file-path API routes into NodeKind::ApiEndpoint nodes
//! by inspecting the filesystem layout under app/ and pages/api/.
//!
//! This module re-exports from `crate::extract::nextjs_routing`. It exists here
//! to make the detection hierarchy visible: consumers triggered by
//! FrameworkDetected live in `consumers/framework/`.
pub use crate::extract::nextjs_routing::*;
