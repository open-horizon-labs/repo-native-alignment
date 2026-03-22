//! Manifest consumer — subscribes to RootDiscovered.
//!
//! Parses package manifests (Cargo.toml, package.json, pyproject.toml, go.mod)
//! and emits Package nodes with dependency edges.
//!
//! This module re-exports from `crate::extract::manifest`. It exists here to
//! make the detection hierarchy visible from the directory structure:
//! consumers triggered by RootDiscovered live in `consumers/root/`.
pub use crate::extract::manifest::*;
