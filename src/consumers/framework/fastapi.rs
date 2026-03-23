//! FastAPI router prefix consumer — subscribes to `FrameworkDetected("fastapi")`.
//!
//! Signals that FastAPI is in use and prefix patching should run. The actual
//! `fastapi_router_prefix_pass` logic runs inside `EnrichmentFinalizer` which has
//! access to the full node set and `root_pairs`. This consumer establishes the
//! event-driven subscription slot per the ADR framework-gated pattern.
//!
//! This module re-exports `FastapiRouterPrefixConsumer` from `crate::extract::consumers`.
//! It exists here to make the detection hierarchy visible: consumers triggered by
//! `FrameworkDetected` live in `consumers/framework/`.
pub use crate::extract::consumers::FastapiRouterPrefixConsumer;
