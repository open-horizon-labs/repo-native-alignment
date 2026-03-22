//! PubSub consumer — subscribes to FrameworkDetected (kafka, celery, pika, redis).
//!
//! Detects message broker publish/subscribe patterns and emits Produces/Consumes
//! edges between producer and consumer nodes.
//!
//! This module re-exports from `crate::extract::pubsub`. It exists here to make
//! the detection hierarchy visible: consumers triggered by FrameworkDetected
//! live in `consumers/framework/`.
pub use crate::extract::pubsub::*;
