//! Consumers of `RootExtracted` events.
//!
//! These run after tree-sitter extraction completes for a root. They operate
//! on the full extracted node set to add edges, detect frameworks, and link
//! related nodes.
//!
//! `framework_detect` is special: it emits `FrameworkDetected` events that
//! trigger consumers in `consumers/framework/`.
pub mod api_link;
pub mod directory_module;
pub mod extractor_config;
pub mod framework_detect;
pub mod import_calls;
pub mod tested_by;
