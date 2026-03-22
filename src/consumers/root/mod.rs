//! Consumers of `RootDiscovered` events.
//!
//! These run once per root at startup before any file extraction.
//! Currently: manifest parsing (Cargo.toml, package.json, pyproject.toml, go.mod).
pub mod manifest;
