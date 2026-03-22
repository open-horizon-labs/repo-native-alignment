//! Tested-by consumer — subscribes to RootExtracted.
//!
//! Detects test functions that follow naming conventions (test_foo tests foo)
//! and emits TestedBy edges between test nodes and the functions they test.
//!
//! The implementation lives in `crate::extract::naming_convention`. This module
//! exists here to make the detection hierarchy visible: consumers triggered by
//! RootExtracted live in `consumers/extraction/`.
pub use crate::extract::naming_convention::tested_by_pass;
