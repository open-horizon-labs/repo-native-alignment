//! Event bus — coordinates extraction pipeline consumers.
//!
//! # Current state
//!
//! This module is a placeholder for the event bus design described in
//! `docs/ADRs/001-event-bus-extraction-pipeline.md`. The `EventBus` trait
//! and in-process implementation will be added in issue #479.
//!
//! # Design
//!
//! ```rust,ignore
//! trait EventBus: Send + Sync {
//!     fn emit(&self, event: ExtractionEvent);
//!     fn register(&mut self, consumer: Box<dyn ExtractionConsumer>);
//! }
//! ```
//!
//! All consumers register at startup (static registration). The bus routes
//! events to matching subscribers at runtime. No consumer knows about other
//! consumers — the bus is the only coupling.
//!
//! See ADR §"Static registration, dynamic routing" for the design rationale.
