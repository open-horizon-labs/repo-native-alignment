//! Content-addressed per-consumer cache keys.
//!
//! # Design
//!
//! Each `ExtractionConsumer` declares a `version() -> u64`. The cache key for a
//! consumer+event pair is `(blake3(event.canonical_bytes()), consumer.version())`.
//!
//! This replaces the single global `EXTRACTION_VERSION` integer:
//!
//! - **Before:** any consumer change bumped `EXTRACTION_VERSION`, forcing every consumer
//!   on every file to re-run on the next scan.
//! - **After:** changing one consumer's `version()` only invalidates that consumer's
//!   cached results. Consumers whose logic has not changed continue to hit the cache.
//!
//! # Cache key semantics
//!
//! ```text
//! cache_key = (blake3(event.canonical_bytes()), consumer_version)
//! ```
//!
//! - Upstream output changes → payload hash changes → downstream cache misses automatically.
//! - Consumer version bumps → only that consumer misses.
//! - Config-driven consumers (`CustomExtractorConsumer`) use `blake3(toml_file_contents)`
//!   as their version — no manual bump needed.
//!
//! # Migration
//!
//! On first run after this lands, all caches will miss (new cache key format). That is
//! expected — everything re-runs once, then incremental extraction becomes per-consumer.

use std::hash::Hash;

/// Cache key for one consumer+event pair.
///
/// Two keys are equal iff the event payload bytes **and** the consumer version match.
/// This is the identity predicate for "has this consumer already processed this event?".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConsumerCacheKey {
    /// `blake3` hash (hex string) of `event.canonical_bytes()`.
    pub payload_hash: String,
    /// Consumer's self-declared version (see `ExtractionConsumer::version()`).
    pub consumer_version: u64,
}

impl ConsumerCacheKey {
    /// Build a cache key from raw bytes and a consumer version.
    pub fn new(payload_bytes: &[u8], consumer_version: u64) -> Self {
        let payload_hash = blake3::hash(payload_bytes).to_hex().to_string();
        Self { payload_hash, consumer_version }
    }
}

/// Fast hasher to convert a `ConsumerCacheKey` to a u64 for use in `HashMap`.
///
/// Not used directly — `ConsumerCacheKey` derives `Hash` and works with any
/// `HashMap<ConsumerCacheKey, V>`. This struct is exposed for callers that
/// need a stable numeric fingerprint (e.g., for LanceDB row keys).
pub fn key_fingerprint(key: &ConsumerCacheKey) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    std::hash::Hasher::finish(&hasher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_bytes_same_version_equal() {
        let k1 = ConsumerCacheKey::new(b"hello world", 42);
        let k2 = ConsumerCacheKey::new(b"hello world", 42);
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_different_bytes_not_equal() {
        let k1 = ConsumerCacheKey::new(b"hello", 1);
        let k2 = ConsumerCacheKey::new(b"world", 1);
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_different_version_not_equal() {
        let k1 = ConsumerCacheKey::new(b"hello", 1);
        let k2 = ConsumerCacheKey::new(b"hello", 2);
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_empty_bytes_zero_version() {
        let k = ConsumerCacheKey::new(b"", 0);
        assert!(!k.payload_hash.is_empty());
    }

    #[test]
    fn test_fingerprint_stable_for_same_key() {
        let k = ConsumerCacheKey::new(b"stable", 7);
        assert_eq!(key_fingerprint(&k), key_fingerprint(&k));
    }

    #[test]
    fn test_fingerprint_differs_for_different_keys() {
        let k1 = ConsumerCacheKey::new(b"a", 1);
        let k2 = ConsumerCacheKey::new(b"b", 1);
        // While collisions are theoretically possible, these specific keys should not collide.
        assert_ne!(key_fingerprint(&k1), key_fingerprint(&k2));
    }

    #[test]
    fn test_config_driven_consumer_version_from_blake3() {
        // Simulate how CustomExtractorConsumer computes its version from file contents.
        let config_v1 = b"[extractor]\nframework = 'nextjs'";
        let config_v2 = b"[extractor]\nframework = 'nextjs'\nsome_new_field = true";

        // Same file bytes → same version.
        let version_a = u64::from_le_bytes(
            blake3::hash(config_v1).as_bytes()[..8].try_into().unwrap()
        );
        let version_b = u64::from_le_bytes(
            blake3::hash(config_v1).as_bytes()[..8].try_into().unwrap()
        );
        assert_eq!(version_a, version_b, "same config bytes must yield same version");

        // Different file bytes → different version.
        let version_c = u64::from_le_bytes(
            blake3::hash(config_v2).as_bytes()[..8].try_into().unwrap()
        );
        assert_ne!(version_a, version_c, "changed config bytes must yield different version");
    }
}
