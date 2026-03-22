//! Consumers of `FrameworkDetected` events.
//!
//! These run only when a specific framework is detected in the repo.
//! Each consumer declares which framework wakes it up via `applies_when()`.
//!
//! Adding a new framework enricher: implement `PostExtractionPass`,
//! add it here, register in `PostExtractionRegistry::with_builtins()`.
pub mod nextjs;
pub mod pubsub;
pub mod websocket;
