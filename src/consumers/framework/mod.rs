//! Consumers of `FrameworkDetected` events.
//!
//! These run only when a specific framework is detected in the repo.
//! Each consumer subscribes to `FrameworkDetected` and filters on `event.framework`.
//!
//! Adding a new framework consumer: implement `ExtractionConsumer`, subscribe to
//! `FrameworkDetected`, filter on the relevant framework name, and register in
//! `build_builtin_bus()`.
pub mod fastapi;
pub mod nextjs;
pub mod pubsub;
pub mod websocket;
