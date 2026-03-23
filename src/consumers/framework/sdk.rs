//! SDK path inference consumer — subscribes to `FrameworkDetected("fastapi")`.
//!
//! Signals that FastAPI is in use and SDK path inference should run. The actual
//! `sdk_path_inference_pass` logic runs inside `EnrichmentFinalizer` which has
//! access to the full node set. This consumer establishes the event-driven
//! subscription slot per the ADR framework-gated pattern.
//!
//! This module re-exports `SdkPathInferenceConsumer` from `crate::extract::consumers`.
//! It exists here to make the detection hierarchy visible: consumers triggered by
//! `FrameworkDetected` live in `consumers/framework/`.
pub use crate::extract::consumers::SdkPathInferenceConsumer;
