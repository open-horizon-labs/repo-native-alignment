// SPDX-License-Identifier: Apache-2.0

//! Production-only compatibility stub for LanceDB 0.31.
//!
//! LanceDB 0.31 declares `lance-testing` as a normal dependency even though it
//! is referenced only from `#[cfg(test)]` modules. Dependencies are not built
//! with their own test configuration when consumed by RNA, so no
//! `lance-testing` API is required here. Upstream approved the equivalent
//! manifest correction in lancedb/lancedb#3661 by moving the dependency to
//! `[dev-dependencies]`.
//!
//! This crate deliberately exports nothing. Its exact 8.0.0 version satisfies
//! LanceDB 0.31's exact Lance dependency set without importing the accidental
//! `pprof -> inferno -> quick-xml 0.26` production chain. Remove this patch once
//! RNA upgrades to a LanceDB release containing the upstream correction.
