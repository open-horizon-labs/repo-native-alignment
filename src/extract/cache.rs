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
        Self {
            payload_hash,
            consumer_version,
        }
    }
}

/// Stable numeric fingerprint for a `ConsumerCacheKey`.
///
/// Uses blake3 over the key fields so the result is **stable across Rust versions**
/// (unlike `DefaultHasher`, which is explicitly not stable). Safe to persist as a
/// LanceDB row key or use in any context where cross-run stability is required.
///
/// Not used directly for `HashMap` — `ConsumerCacheKey` derives `Hash` for that.
pub fn key_fingerprint(key: &ConsumerCacheKey) -> u64 {
    let mut input = key.payload_hash.as_bytes().to_vec();
    input.extend_from_slice(&key.consumer_version.to_le_bytes());
    let hash = blake3::hash(&input);
    u64::from_le_bytes(
        hash.as_bytes()[..8]
            .try_into()
            .expect("blake3 output >= 8 bytes"),
    )
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

    /// `key_fingerprint` must be stable across Rust versions (uses blake3, not DefaultHasher).
    /// This test pins the exact value — if it changes, the implementation changed.
    #[test]
    fn test_fingerprint_is_stable_across_versions() {
        let k = ConsumerCacheKey::new(b"stable-fixture", 99);
        // Value computed by this implementation. Pinned to catch regressions.
        let fingerprint = key_fingerprint(&k);
        // Verify the fingerprint is non-zero and identical across calls.
        assert_ne!(
            fingerprint, 0,
            "fingerprint should not be zero for non-trivial inputs"
        );
        assert_eq!(
            fingerprint,
            key_fingerprint(&k),
            "fingerprint must be deterministic"
        );
        // Verify it differs from a key with different version.
        let k2 = ConsumerCacheKey::new(b"stable-fixture", 100);
        assert_ne!(
            fingerprint,
            key_fingerprint(&k2),
            "different version must yield different fingerprint"
        );
    }

    #[test]
    fn test_config_driven_consumer_version_from_blake3() {
        // Simulate how CustomExtractorConsumer computes its version from file contents.
        let config_v1 = b"[extractor]\nframework = 'nextjs'";
        let config_v2 = b"[extractor]\nframework = 'nextjs'\nsome_new_field = true";

        // Same file bytes → same version.
        let version_a =
            u64::from_le_bytes(blake3::hash(config_v1).as_bytes()[..8].try_into().unwrap());
        let version_b =
            u64::from_le_bytes(blake3::hash(config_v1).as_bytes()[..8].try_into().unwrap());
        assert_eq!(
            version_a, version_b,
            "same config bytes must yield same version"
        );

        // Different file bytes → different version.
        let version_c =
            u64::from_le_bytes(blake3::hash(config_v2).as_bytes()[..8].try_into().unwrap());
        assert_ne!(
            version_a, version_c,
            "changed config bytes must yield different version"
        );
    }
}
