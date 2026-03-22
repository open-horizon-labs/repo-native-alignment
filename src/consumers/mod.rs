//! Extraction pipeline consumers, organized by the event that triggers them.
//!
//! The directory structure IS the documentation — you can read the detection
//! hierarchy from `ls`:
//!
//! ```text
//! consumers/
//!   root/        — subscribe to RootDiscovered
//!   extraction/  — subscribe to RootExtracted  (framework_detect emits FrameworkDetected)
//!   language/    — subscribe to LanguageDetected (LSP enrichers)
//!   framework/   — subscribe to FrameworkDetected
//!   completion/  — subscribe to PassesComplete
//! ```
//!
//! Each module re-exports its implementation from `crate::extract`. The
//! implementation lives in `extract/` until the full event bus (#479) is in
//! place; at that point the implementations will move here and `extract/`
//! will become a pure trait/registry module.
//!
//! See `docs/ADRs/001-event-bus-extraction-pipeline.md` for the full design.
pub mod completion;
pub mod extraction;
pub mod framework;
pub mod language;
pub mod root;
