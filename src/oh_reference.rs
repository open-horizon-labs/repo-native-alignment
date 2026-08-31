//! Advisory resolution for stable Open Horizons references.
//!
//! This module is intentionally a one-way, identity-only integration. It sends
//! one canonical `oh://` URI and an optional expected kind to the authorized OH
//! resolver, and caches only the resolver's minimal identity/lifecycle/version
//! projection. Repository graph nodes, edges, bodies, embeddings, and outbox
//! records are never inputs to the transport.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CACHE_SCHEMA_VERSION: u8 = 1;
const MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 10_000;
const MAX_RESPONSE_BYTES: usize = 32 * 1024;
const MAX_REFERENCE_INPUT_BYTES: usize = 8 * 1024;
const OVERSIZE_REFERENCE_MARKER: &str = "<oversize-oh-reference>";
const DEFAULT_CACHE_TTL_SECONDS: u64 = 24 * 60 * 60;
pub const MAX_CACHE_TTL_SECONDS: u64 = 24 * 60 * 60;
pub const MAX_REFERENCE_DECLARATIONS: usize = 256;
const MAX_DISCOVERY_FILES: usize = 10_000;
const MAX_DISCOVERY_FILE_BYTES: u64 = 1024 * 1024;
const MAX_DISCOVERY_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DISCOVERY_ISSUES: usize = 64;
const MAX_BATCH_DURATION: Duration = Duration::from_secs(12);
const CACHE_LOCK_TIMEOUT: Duration = Duration::from_secs(11);
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub const LEGACY_CACHE_RELATIVE_PATH: &str = ".oh/.cache/oh-reference-resolutions-v1.json";
pub const DEFAULT_API_KEY_ENV: &str = "OPEN_HORIZONS_API_KEY";
pub const DEFAULT_RESOLVER_URL_ENV: &str = "OPEN_HORIZONS_RESOLVER_URL";

pub const OH_REFERENCE_KINDS: &[&str] = &[
    "context",
    "endeavor",
    "metis",
    "guardrail",
    "dive_pack",
    "log",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OhReferenceKind {
    Context,
    Endeavor,
    Metis,
    Guardrail,
    DivePack,
    Log,
}

impl OhReferenceKind {
    /// Returns the canonical URI/frontmatter spelling for this entity kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Endeavor => "endeavor",
            Self::Metis => "metis",
            Self::Guardrail => "guardrail",
            Self::DivePack => "dive_pack",
            Self::Log => "log",
        }
    }

    /// Parses a canonical URI/frontmatter kind without accepting aliases.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "context" => Some(Self::Context),
            "endeavor" => Some(Self::Endeavor),
            "metis" => Some(Self::Metis),
            "guardrail" => Some(Self::Guardrail),
            "dive_pack" => Some(Self::DivePack),
            "log" => Some(Self::Log),
            _ => None,
        }
    }
}

impl std::fmt::Display for OhReferenceKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OhReference {
    pub kind: OhReferenceKind,
    pub id: String,
    pub uri: String,
}

impl OhReference {
    /// Parses and canonicalizes an `oh://v1/<kind>/<id>` reference.
    pub fn parse(value: &str) -> Result<Self> {
        let remainder = value
            .strip_prefix("oh://v1/")
            .ok_or_else(|| anyhow!("unsupported OH reference URI"))?;
        if remainder.contains(['?', '#']) {
            bail!("OH reference URI must not contain a query or fragment");
        }
        let mut segments = remainder.split('/');
        let kind = segments
            .next()
            .and_then(OhReferenceKind::parse)
            .ok_or_else(|| anyhow!("unsupported OH reference kind"))?;
        let encoded_id = segments
            .next()
            .filter(|segment| !segment.is_empty())
            .ok_or_else(|| anyhow!("missing OH reference identifier"))?;
        if segments.next().is_some() {
            bail!("OH reference URI must contain exactly one identifier segment");
        }
        let id = percent_decode(encoded_id)?;
        // The producer contract is TypeScript, where String.length counts
        // UTF-16 code units rather than Unicode scalar values.
        if id.is_empty() || id.contains('/') || id.encode_utf16().count() > 512 {
            bail!("invalid OH reference identifier");
        }
        let canonical = format!("oh://v1/{}/{}", kind.as_str(), percent_encode(&id));
        if value != canonical {
            bail!("OH reference URI is not canonical");
        }
        Ok(Self {
            kind,
            id,
            uri: canonical,
        })
    }
}

/// Percent-decodes one URI path segment and rejects malformed UTF-8.
fn percent_decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                bail!("invalid percent escape in OH reference identifier");
            }
            let high = hex(bytes[index + 1])?;
            let low = hex(bytes[index + 2])?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).context("OH reference identifier is not UTF-8")
}

/// Converts one ASCII hexadecimal digit to its numeric value.
fn hex(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid percent escape in OH reference identifier"),
    }
}

/// Encodes an OH identifier using the producer's URI component rules.
fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            encoded.push(char::from(*byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisoryState {
    Confirmed,
    Unresolved,
    Retired,
    TypeMismatch,
    Stale,
    Unauthorized,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionSource {
    Network,
    Cache,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryResolution {
    pub reference: String,
    pub state: AdvisoryState,
    pub source: ResolutionSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<OhReferenceKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<OhReferenceLifecycle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at_unix_seconds: Option<u64>,
}

impl AdvisoryResolution {
    /// Builds a result that contains no cached or network-derived metadata.
    fn state_only(reference: impl Into<String>, state: AdvisoryState) -> Self {
        Self {
            reference: reference.into(),
            state,
            source: ResolutionSource::None,
            kind: None,
            lifecycle: None,
            version: None,
            checked_at_unix_seconds: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OhReferenceLifecycle {
    Active,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedResolution {
    reference: String,
    kind: OhReferenceKind,
    lifecycle: OhReferenceLifecycle,
    version: u64,
    checked_at_unix_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResolutionCache {
    schema_version: u8,
    entries: BTreeMap<String, CachedResolution>,
}

impl Default for ResolutionCache {
    fn default() -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

impl ResolutionCache {
    /// Loads and validates a private, size-bounded cache file.
    fn load(path: &Path) -> Result<Self> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        validate_private_regular_file(&metadata)?;
        if metadata.len() > MAX_CACHE_BYTES {
            bail!("OH reference cache exceeds {MAX_CACHE_BYTES} bytes");
        }
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let cache: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", path.display()))?;
        if cache.schema_version != CACHE_SCHEMA_VERSION {
            bail!("unsupported OH reference cache schema");
        }
        if cache.entries.len() > MAX_CACHE_ENTRIES {
            bail!("OH reference cache contains too many entries");
        }
        for (key, entry) in &cache.entries {
            let reference = OhReference::parse(key)?;
            if entry.reference != *key || entry.kind != reference.kind || entry.version == 0 {
                bail!("invalid OH reference cache entry");
            }
        }
        Ok(cache)
    }

    /// Removes the oldest entries until the cache satisfies its hard bound.
    fn prune_to_limit(&mut self) {
        if self.entries.len() <= MAX_CACHE_ENTRIES {
            return;
        }
        let mut oldest: Vec<_> = self
            .entries
            .iter()
            .map(|(key, entry)| (entry.checked_at_unix_seconds, key.clone()))
            .collect();
        oldest.sort();
        for (_, key) in oldest
            .into_iter()
            .take(self.entries.len() - MAX_CACHE_ENTRIES)
        {
            self.entries.remove(&key);
        }
    }

    /// Atomically persists a private cache after enforcing its entry bound.
    fn persist(&mut self, path: &Path) -> Result<()> {
        self.prune_to_limit();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or_else(|| anyhow!("OH reference cache path has no parent"))?;
        secure_cache_directory(parent)?;
        match fs::symlink_metadata(path) {
            Ok(metadata) => validate_private_regular_file(&metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        }
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".oh-reference-resolutions-v1.tmp-{}-{sequence}",
            std::process::id()
        ));
        let bytes = serde_json::to_vec_pretty(self)?;
        if bytes.len() as u64 > MAX_CACHE_BYTES {
            bail!("OH reference cache exceeds {MAX_CACHE_BYTES} bytes");
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            let mut file = options
                .open(&temporary)
                .with_context(|| format!("creating {}", temporary.display()))?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temporary, path)
                .with_context(|| format!("replacing {}", path.display()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

/// Rejects symlinks, non-files, foreign owners, and permissive Unix modes.
fn validate_private_regular_file(metadata: &fs::Metadata) -> Result<()> {
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("OH reference cache must be a regular non-symlink file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        validate_unix_owner_and_mode(metadata.uid(), metadata.mode(), current_effective_uid())?;
    }
    Ok(())
}

#[cfg(unix)]
/// Returns the effective Unix user ID used for cache ownership checks.
fn current_effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() }
}

#[cfg(unix)]
/// Enforces current-user ownership and user-only cache permissions.
fn validate_unix_owner_and_mode(owner: u32, mode: u32, effective_uid: u32) -> Result<()> {
    if owner != effective_uid {
        bail!("OH reference cache is not owned by the current user");
    }
    if mode & 0o077 != 0 {
        bail!("OH reference cache must not be group/world accessible");
    }
    Ok(())
}

/// Creates or verifies the non-symlink, user-private cache directory.
fn secure_cache_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))?;
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("inspecting {}", path.display()))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!("OH reference cache parent must be a non-symlink directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != current_effective_uid() {
            bail!("OH reference cache directory is not owned by the current user");
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("securing {}", path.display()))?;
    }
    Ok(())
}

struct CacheLock {
    file: File,
}

impl CacheLock {
    /// Acquires the cross-process cache lock within a bounded wait.
    async fn acquire(cache_path: &Path) -> Result<Self> {
        let parent = cache_path
            .parent()
            .ok_or_else(|| anyhow!("OH reference cache path has no parent"))?;
        secure_cache_directory(parent)?;
        let lock_path = cache_path.with_extension("lock");
        match fs::symlink_metadata(&lock_path) {
            Ok(metadata) => validate_private_regular_file(&metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", lock_path.display()));
            }
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(&lock_path)
            .with_context(|| format!("opening {}", lock_path.display()))?;
        validate_private_regular_file(&file.metadata()?)?;

        let deadline = tokio::time::Instant::now() + CACHE_LOCK_TIMEOUT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if tokio::time::Instant::now() >= deadline {
                        bail!("timed out waiting for OH reference cache lock");
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(error).context("locking OH reference cache");
                }
            }
        }
    }
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[async_trait]
pub trait ReferenceTransport: Send + Sync {
    /// Resolves one canonical identity without sending repository content.
    async fn resolve(
        &self,
        endpoint: &str,
        api_key: &str,
        reference: &OhReference,
        expected_kind: Option<OhReferenceKind>,
    ) -> Result<TransportResponse>;
}

#[derive(Debug, Clone)]
pub struct ReqwestReferenceTransport {
    client: reqwest::Client,
}

impl Default for ReqwestReferenceTransport {
    /// Builds the bounded, no-redirect HTTP client used for identity lookup.
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                // An API key must never follow a repository- or endpoint-
                // controlled redirect to a different destination.
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(10))
                .build()
                .expect("static OH resolver HTTP client configuration is valid"),
        }
    }
}

#[derive(Serialize)]
struct ResolverRequest<'a> {
    reference: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_kind: Option<OhReferenceKind>,
}

#[async_trait]
impl ReferenceTransport for ReqwestReferenceTransport {
    /// Sends the minimal resolver request and bounds the response body.
    async fn resolve(
        &self,
        endpoint: &str,
        api_key: &str,
        reference: &OhReference,
        expected_kind: Option<OhReferenceKind>,
    ) -> Result<TransportResponse> {
        validate_api_key(api_key)?;
        let endpoint = validated_resolver_url(endpoint)?;
        let mut response = self
            .client
            .post(endpoint)
            .bearer_auth(api_key)
            .json(&ResolverRequest {
                reference: &reference.uri,
                expected_kind,
            })
            .send()
            .await
            .context("OH resolver request failed")?;
        let status = response.status().as_u16();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            bail!("OH resolver response exceeds {MAX_RESPONSE_BYTES} bytes");
        }
        let mut body = Vec::with_capacity(
            response
                .content_length()
                .unwrap_or_default()
                .min(MAX_RESPONSE_BYTES as u64) as usize,
        );
        while let Some(chunk) = response
            .chunk()
            .await
            .context("reading OH resolver response")?
        {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                bail!("OH resolver response exceeds {MAX_RESPONSE_BYTES} bytes");
            }
            body.extend_from_slice(&chunk);
        }
        Ok(TransportResponse { status, body })
    }
}

/// Checks the retained API-key shape without logging or persisting it.
fn validate_api_key(api_key: &str) -> Result<()> {
    if !api_key.starts_with("ak_") || api_key.chars().any(char::is_whitespace) {
        bail!("Open Horizons API key is not a valid existing ak_ credential");
    }
    Ok(())
}

/// Validates resolver URL syntax and the HTTPS-or-loopback transport rule.
fn validated_resolver_url(endpoint: &str) -> Result<reqwest::Url> {
    let endpoint = reqwest::Url::parse(endpoint).context("invalid OH resolver URL")?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.username() != ""
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        bail!("OH resolver URL must be an http(s) URL without credentials, query, or fragment");
    }
    if endpoint.scheme() != "https" && !resolver_host_is_loopback(&endpoint) {
        bail!("OH resolver URL must use HTTPS except on loopback");
    }
    Ok(endpoint)
}

/// Recognizes loopback URL hosts without depending on their serialized form.
///
/// In particular, `Url::host_str()` retains brackets around IPv6 literals,
/// while `Url::host()` exposes the parsed address for a reliable `::1` check.
fn resolver_host_is_loopback(endpoint: &reqwest::Url) -> bool {
    endpoint.host().is_some_and(|host| match host {
        url::Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
        url::Host::Ipv4(address) => address.is_loopback(),
        url::Host::Ipv6(address) => address.is_loopback(),
    })
}

/// Produces the stable endpoint identity used to isolate authorized caches.
fn normalized_resolver_endpoint(endpoint: &str) -> Result<String> {
    let mut endpoint = validated_resolver_url(endpoint)?;
    if endpoint.path() != "/" {
        let normalized_path = endpoint.path().trim_end_matches('/').to_string();
        endpoint.set_path(&normalized_path);
    }
    Ok(endpoint.to_string())
}

/// Returns a lowercase SHA-256 digest for cache namespace derivation.
fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Derives a repo-, authority-, credential-, and user-isolated cache path.
fn authorized_cache_path(
    repo_root: &Path,
    endpoint: Option<&str>,
    api_key: Option<&str>,
    cache_base: Option<&Path>,
) -> Result<PathBuf> {
    let endpoint = normalized_resolver_endpoint(
        endpoint.ok_or_else(|| anyhow!("OH resolver endpoint is not configured"))?,
    )?;
    let api_key = api_key.ok_or_else(|| anyhow!("OH API key is not configured"))?;
    validate_api_key(api_key)?;
    let repo_identity = repo_root
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", repo_root.display()))?;
    let cache_base = match cache_base {
        Some(path) => path.to_path_buf(),
        None => dirs::cache_dir().ok_or_else(|| anyhow!("user cache directory is unavailable"))?,
    };
    Ok(cache_base
        .join("repo-native-alignment")
        .join("oh-references-v1")
        .join(sha256_hex(endpoint.as_bytes()))
        .join(sha256_hex(api_key.as_bytes()))
        .join(format!(
            "{}.json",
            sha256_hex(repo_identity.to_string_lossy().as_bytes())
        )))
}

#[derive(Debug, Clone)]
pub struct AdvisoryResolver<T> {
    transport: T,
    endpoint: Option<String>,
    api_key: Option<String>,
    cache_path: Option<PathBuf>,
    cache_ttl: Duration,
}

impl<T: ReferenceTransport> AdvisoryResolver<T> {
    /// Constructs a resolver using the platform user-cache directory.
    pub fn new(
        transport: T,
        endpoint: Option<String>,
        api_key: Option<String>,
        repo_root: &Path,
        cache_ttl: Duration,
    ) -> Self {
        Self::new_with_cache_base(transport, endpoint, api_key, repo_root, cache_ttl, None)
    }

    /// Constructs a resolver with an explicit cache base for isolated tests.
    fn new_with_cache_base(
        transport: T,
        endpoint: Option<String>,
        api_key: Option<String>,
        repo_root: &Path,
        cache_ttl: Duration,
        cache_base: Option<&Path>,
    ) -> Self {
        let cache_path = authorized_cache_path(
            repo_root,
            endpoint.as_deref(),
            api_key.as_deref(),
            cache_base,
        )
        .map_err(|error| tracing::warn!("OH reference cache unavailable: {error:#}"))
        .ok();
        Self {
            transport,
            endpoint,
            api_key,
            cache_path,
            cache_ttl: cache_ttl.min(Duration::from_secs(MAX_CACHE_TTL_SECONDS)),
        }
    }

    /// Resolves one identity using the current wall-clock timestamp.
    pub async fn resolve(
        &self,
        uri: &str,
        expected_kind: Option<OhReferenceKind>,
        offline: bool,
    ) -> AdvisoryResolution {
        self.resolve_at(uri, expected_kind, offline, unix_now())
            .await
    }

    /// Resolves one identity at a supplied timestamp for deterministic policy tests.
    async fn resolve_at(
        &self,
        uri: &str,
        expected_kind: Option<OhReferenceKind>,
        offline: bool,
        now: u64,
    ) -> AdvisoryResolution {
        if uri.len() > MAX_REFERENCE_INPUT_BYTES {
            return AdvisoryResolution::state_only(
                OVERSIZE_REFERENCE_MARKER,
                AdvisoryState::Unresolved,
            );
        }
        let reference = match OhReference::parse(uri) {
            Ok(reference) => reference,
            Err(_) => return AdvisoryResolution::state_only(uri, AdvisoryState::Unresolved),
        };
        if expected_kind.is_some_and(|kind| kind != reference.kind) {
            return AdvisoryResolution {
                reference: reference.uri,
                state: AdvisoryState::TypeMismatch,
                source: ResolutionSource::None,
                kind: Some(reference.kind),
                lifecycle: None,
                version: None,
                checked_at_unix_seconds: None,
            };
        }

        let Some(cache_path) = &self.cache_path else {
            return AdvisoryResolution::state_only(reference.uri, AdvisoryState::Unavailable);
        };
        let _lock = if offline {
            None
        } else {
            match CacheLock::acquire(cache_path).await {
                Ok(lock) => Some(lock),
                Err(error) => {
                    tracing::warn!("OH advisory resolver cache lock unavailable: {error:#}");
                    return ResolutionCache::load(cache_path)
                        .ok()
                        .and_then(|cache| {
                            cached_resolution(&cache, &reference, now, self.cache_ttl)
                        })
                        .unwrap_or_else(|| {
                            AdvisoryResolution::state_only(
                                reference.uri,
                                AdvisoryState::Unavailable,
                            )
                        });
                }
            }
        };
        let mut cache = ResolutionCache::load(cache_path).unwrap_or_else(|error| {
            tracing::warn!("ignoring invalid OH reference cache: {error:#}");
            ResolutionCache::default()
        });
        if offline {
            return cached_resolution(&cache, &reference, now, self.cache_ttl).unwrap_or_else(
                || AdvisoryResolution::state_only(reference.uri, AdvisoryState::Unavailable),
            );
        }
        let (Some(endpoint), Some(api_key)) = (&self.endpoint, &self.api_key) else {
            return cached_resolution(&cache, &reference, now, self.cache_ttl).unwrap_or_else(
                || AdvisoryResolution::state_only(reference.uri, AdvisoryState::Unavailable),
            );
        };

        let response = match self
            .transport
            .resolve(endpoint, api_key, &reference, expected_kind)
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!("OH advisory resolver unavailable: {error:#}");
                return cached_resolution(&cache, &reference, now, self.cache_ttl).unwrap_or_else(
                    || AdvisoryResolution::state_only(reference.uri, AdvisoryState::Unavailable),
                );
            }
        };
        match response.status {
            200 => match parse_success(&reference, &response.body, now) {
                Ok((entry, resolution)) => {
                    cache.entries.insert(reference.uri.clone(), entry);
                    if let Err(error) = cache.persist(cache_path) {
                        tracing::warn!("failed to persist OH reference cache: {error:#}");
                    }
                    resolution
                }
                Err(error) => {
                    tracing::warn!("invalid OH resolver response: {error:#}");
                    cached_resolution(&cache, &reference, now, self.cache_ttl).unwrap_or_else(
                        || {
                            AdvisoryResolution::state_only(
                                reference.uri,
                                AdvisoryState::Unavailable,
                            )
                        },
                    )
                }
            },
            404 => {
                evict_cached_reference(&mut cache, cache_path, &reference.uri);
                AdvisoryResolution::state_only(reference.uri, AdvisoryState::Unresolved)
            }
            409 => AdvisoryResolution {
                reference: reference.uri,
                state: AdvisoryState::TypeMismatch,
                source: ResolutionSource::Network,
                kind: Some(reference.kind),
                lifecycle: None,
                version: None,
                checked_at_unix_seconds: Some(now),
            },
            401 | 403 => {
                // Revoked/insufficient access must not continue exposing even the
                // minimal cached projection to this caller.
                evict_cached_reference(&mut cache, cache_path, &reference.uri);
                AdvisoryResolution::state_only(reference.uri, AdvisoryState::Unauthorized)
            }
            _ => cached_resolution(&cache, &reference, now, self.cache_ttl).unwrap_or_else(|| {
                AdvisoryResolution::state_only(reference.uri, AdvisoryState::Unavailable)
            }),
        }
    }
}

/// Removes authorization-sensitive cached data after an access denial.
fn evict_cached_reference(cache: &mut ResolutionCache, path: &Path, reference: &str) {
    if cache.entries.remove(reference).is_none() {
        return;
    }
    if let Err(error) = cache.persist(path) {
        tracing::warn!("failed to persist OH reference cache eviction: {error:#}");
        // This is disposable advisory state. Removing the entire cache is safer
        // than allowing a revoked/deleted identity to reappear on the next run.
        if let Err(remove_error) = fs::remove_file(path)
            && remove_error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                "failed to remove OH reference cache after eviction failure: {remove_error}"
            );
        }
    }
}

#[derive(Debug, Deserialize)]
struct ResolverSuccess {
    contract_version: String,
    outcome: String,
    reference: String,
    kind: OhReferenceKind,
    lifecycle: OhReferenceLifecycle,
    version: u64,
}

/// Validates a successful resolver projection against the requested identity.
fn parse_success(
    requested: &OhReference,
    body: &[u8],
    now: u64,
) -> Result<(CachedResolution, AdvisoryResolution)> {
    let response: ResolverSuccess = serde_json::from_slice(body)?;
    if response.contract_version != "v1"
        || response.reference != requested.uri
        || response.kind != requested.kind
        || response.version == 0
    {
        bail!("OH resolver response does not match the requested identity");
    }
    let state = match (response.outcome.as_str(), response.lifecycle) {
        ("resolved", OhReferenceLifecycle::Active) => AdvisoryState::Confirmed,
        ("retired", OhReferenceLifecycle::Retired) => AdvisoryState::Retired,
        _ => bail!("OH resolver response has inconsistent outcome/lifecycle"),
    };
    let entry = CachedResolution {
        reference: response.reference.clone(),
        kind: response.kind,
        lifecycle: response.lifecycle,
        version: response.version,
        checked_at_unix_seconds: now,
    };
    Ok((
        entry,
        AdvisoryResolution {
            reference: response.reference,
            state,
            source: ResolutionSource::Network,
            kind: Some(response.kind),
            lifecycle: Some(response.lifecycle),
            version: Some(response.version),
            checked_at_unix_seconds: Some(now),
        },
    ))
}

/// Projects a cached record into a fresh, stale, or type-mismatch advisory result.
fn cached_resolution(
    cache: &ResolutionCache,
    reference: &OhReference,
    now: u64,
    ttl: Duration,
) -> Option<AdvisoryResolution> {
    let entry = cache.entries.get(&reference.uri)?;
    let age = now.saturating_sub(entry.checked_at_unix_seconds);
    let fresh = entry.checked_at_unix_seconds <= now && age <= ttl.as_secs();
    let state = if fresh {
        match entry.lifecycle {
            OhReferenceLifecycle::Active => AdvisoryState::Confirmed,
            OhReferenceLifecycle::Retired => AdvisoryState::Retired,
        }
    } else {
        AdvisoryState::Stale
    };
    Some(AdvisoryResolution {
        reference: entry.reference.clone(),
        state,
        source: ResolutionSource::Cache,
        kind: Some(entry.kind),
        lifecycle: Some(entry.lifecycle),
        version: Some(entry.version),
        checked_at_unix_seconds: Some(entry.checked_at_unix_seconds),
    })
}

/// Returns the current Unix timestamp, saturating pre-epoch clocks to zero.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenHorizonsReferenceConfig {
    #[serde(default = "default_cache_ttl_seconds")]
    pub cache_ttl_seconds: u64,
}

impl Default for OpenHorizonsReferenceConfig {
    fn default() -> Self {
        Self {
            cache_ttl_seconds: default_cache_ttl_seconds(),
        }
    }
}

/// Supplies the serde/default cache freshness interval.
const fn default_cache_ttl_seconds() -> u64 {
    DEFAULT_CACHE_TTL_SECONDS
}

impl OpenHorizonsReferenceConfig {
    /// Loads optional repository configuration and falls back safely on errors.
    pub fn load(repo_root: &Path) -> Self {
        let path = repo_root.join(".oh/config.toml");
        let Ok(content) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(value) = toml::from_str::<toml::Value>(&content) else {
            tracing::warn!(
                "cannot parse {} for OH reference configuration",
                path.display()
            );
            return Self::default();
        };
        value
            .get("open_horizons_references")
            .and_then(|section| section.clone().try_into().ok())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReferenceDeclaration {
    pub source_file: PathBuf,
    pub reference: String,
    pub expected_kind: OhReferenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeclarationResolution {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<PathBuf>,
    pub expected_kind: OhReferenceKind,
    pub resolution: AdvisoryResolution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisoryIssueReason {
    UnreadableFile,
    NonUtf8File,
    OversizeFile,
    FileLimit,
    TotalByteLimit,
    DeclarationLimit,
    OversizeReference,
    BatchDeadline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdvisoryIssue {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<PathBuf>,
    pub state: AdvisoryState,
    pub reason: AdvisoryIssueReason,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReferenceDiscovery {
    pub declarations: Vec<ReferenceDeclaration>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<AdvisoryIssue>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ResolutionBatch {
    pub resolutions: Vec<DeclarationResolution>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<AdvisoryIssue>,
    pub truncated: bool,
}

/// Bound explicit untrusted CLI/MCP inputs before URI parsing or declaration
/// mapping. Oversized raw values are dropped and replaced with a fixed marker;
/// diagnostics contain reason codes only and never echo the input.
pub fn preflight_explicit_references(
    mut references: Vec<String>,
    explicit_kind: Option<OhReferenceKind>,
) -> ReferenceDiscovery {
    let mut discovery = ReferenceDiscovery::default();
    if references.len() > MAX_REFERENCE_DECLARATIONS {
        references.truncate(MAX_REFERENCE_DECLARATIONS);
        discovery.truncated = true;
        push_issue(
            &mut discovery.issues,
            AdvisoryIssue {
                source_file: None,
                state: AdvisoryState::Unavailable,
                reason: AdvisoryIssueReason::DeclarationLimit,
            },
        );
    }
    references.shrink_to_fit();

    let mut oversized = false;
    for reference in &mut references {
        if reference.len() > MAX_REFERENCE_INPUT_BYTES {
            *reference = OVERSIZE_REFERENCE_MARKER.to_string();
            oversized = true;
        }
    }
    if oversized {
        discovery.truncated = true;
        push_issue(
            &mut discovery.issues,
            AdvisoryIssue {
                source_file: None,
                state: AdvisoryState::Unavailable,
                reason: AdvisoryIssueReason::OversizeReference,
            },
        );
    }

    discovery.declarations = references
        .into_iter()
        .map(|reference| {
            let inferred_kind = OhReference::parse(&reference)
                .map(|parsed| parsed.kind)
                // Malformed input resolves as `unresolved`; this fallback is
                // never used to contact the endpoint.
                .unwrap_or(OhReferenceKind::Context);
            ReferenceDeclaration {
                source_file: PathBuf::new(),
                reference,
                expected_kind: explicit_kind.unwrap_or(inferred_kind),
            }
        })
        .collect();
    discovery
}

/// Shared CLI/MCP service seam. Equal identity/kind pairs are resolved once per
/// invocation, while every declaring source retains its own advisory result.
pub async fn resolve_declarations<T: ReferenceTransport>(
    resolver: &AdvisoryResolver<T>,
    discovery: ReferenceDiscovery,
    explicit_kind: Option<OhReferenceKind>,
    offline: bool,
) -> ResolutionBatch {
    resolve_declarations_with_deadline(
        resolver,
        discovery.declarations,
        explicit_kind,
        offline,
        discovery.issues,
        discovery.truncated,
        MAX_BATCH_DURATION,
    )
    .await
}

/// Executes a deduplicated batch while preserving per-declaration results.
async fn resolve_declarations_with_deadline<T: ReferenceTransport>(
    resolver: &AdvisoryResolver<T>,
    declarations: Vec<ReferenceDeclaration>,
    explicit_kind: Option<OhReferenceKind>,
    offline: bool,
    mut issues: Vec<AdvisoryIssue>,
    mut truncated: bool,
    max_duration: Duration,
) -> ResolutionBatch {
    let declaration_count = declarations.len();
    let declarations = declarations
        .into_iter()
        .take(MAX_REFERENCE_DECLARATIONS)
        .collect::<Vec<_>>();
    if declaration_count > declarations.len() {
        truncated = true;
        push_issue(
            &mut issues,
            AdvisoryIssue {
                source_file: None,
                state: AdvisoryState::Unavailable,
                reason: AdvisoryIssueReason::DeclarationLimit,
            },
        );
    }
    let mut resolved: BTreeMap<(String, OhReferenceKind), AdvisoryResolution> = BTreeMap::new();
    let mut output = Vec::with_capacity(declarations.len());
    let deadline = tokio::time::Instant::now() + max_duration;
    for mut declaration in declarations {
        if declaration.reference.len() > MAX_REFERENCE_INPUT_BYTES {
            declaration.reference = OVERSIZE_REFERENCE_MARKER.to_string();
        }
        let expected_kind = explicit_kind.unwrap_or(declaration.expected_kind);
        let key = (declaration.reference.clone(), expected_kind);
        let resolution = match resolved.get(&key) {
            Some(resolution) => resolution.clone(),
            None => {
                let now = tokio::time::Instant::now();
                let resolution = if now >= deadline {
                    AdvisoryResolution::state_only(
                        declaration.reference.clone(),
                        AdvisoryState::Unavailable,
                    )
                } else {
                    tokio::time::timeout(
                        deadline - now,
                        resolver.resolve(&declaration.reference, Some(expected_kind), offline),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        AdvisoryResolution::state_only(
                            declaration.reference.clone(),
                            AdvisoryState::Unavailable,
                        )
                    })
                };
                resolved.insert(key, resolution.clone());
                resolution
            }
        };
        output.push(DeclarationResolution {
            source_file: (!declaration.source_file.as_os_str().is_empty())
                .then_some(declaration.source_file),
            expected_kind,
            resolution,
        });
    }
    if tokio::time::Instant::now() >= deadline {
        truncated = true;
        push_issue(
            &mut issues,
            AdvisoryIssue {
                source_file: None,
                state: AdvisoryState::Unavailable,
                reason: AdvisoryIssueReason::BatchDeadline,
            },
        );
    }
    ResolutionBatch {
        resolutions: output,
        issues,
        truncated,
    }
}

#[derive(Deserialize)]
struct ReferenceFrontmatter {
    rna: Option<ReferenceNode>,
}

#[derive(Deserialize)]
struct ReferenceNode {
    #[serde(default)]
    relationships: Vec<ReferenceRelationship>,
}

#[derive(Deserialize)]
struct ReferenceRelationship {
    target: ReferenceTarget,
}

#[derive(Deserialize)]
struct ReferenceTarget {
    kind: String,
    #[serde(default)]
    uri: Option<String>,
}

/// Discover declared OH targets without resolving or transmitting anything.
pub fn collect_reference_declarations(repo_root: &Path) -> Result<ReferenceDiscovery> {
    let mut declarations = Vec::new();
    let mut issues = Vec::new();
    let mut total_bytes = 0_u64;
    let (paths, files_truncated) =
        crate::walk::walk_repo_files_bounded(repo_root, &["md"], MAX_DISCOVERY_FILES)?;
    let mut truncated = files_truncated;
    if files_truncated {
        push_issue(
            &mut issues,
            AdvisoryIssue {
                source_file: None,
                state: AdvisoryState::Unavailable,
                reason: AdvisoryIssueReason::FileLimit,
            },
        );
    }
    'files: for path in paths {
        let source_file = path.strip_prefix(repo_root).unwrap_or(&path).to_path_buf();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                push_issue(
                    &mut issues,
                    AdvisoryIssue {
                        source_file: Some(source_file),
                        state: AdvisoryState::Unavailable,
                        reason: AdvisoryIssueReason::UnreadableFile,
                    },
                );
                continue;
            }
        };
        if metadata.len() > MAX_DISCOVERY_FILE_BYTES {
            push_issue(
                &mut issues,
                AdvisoryIssue {
                    source_file: Some(source_file),
                    state: AdvisoryState::Unavailable,
                    reason: AdvisoryIssueReason::OversizeFile,
                },
            );
            continue;
        }
        if total_bytes.saturating_add(metadata.len()) > MAX_DISCOVERY_TOTAL_BYTES {
            truncated = true;
            push_issue(
                &mut issues,
                AdvisoryIssue {
                    source_file: None,
                    state: AdvisoryState::Unavailable,
                    reason: AdvisoryIssueReason::TotalByteLimit,
                },
            );
            break;
        }
        let bytes = match File::open(&path).and_then(|file| {
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            file.take(MAX_DISCOVERY_FILE_BYTES + 1)
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        }) {
            Ok(bytes) => bytes,
            Err(_) => {
                push_issue(
                    &mut issues,
                    AdvisoryIssue {
                        source_file: Some(source_file),
                        state: AdvisoryState::Unavailable,
                        reason: AdvisoryIssueReason::UnreadableFile,
                    },
                );
                continue;
            }
        };
        if bytes.len() as u64 > MAX_DISCOVERY_FILE_BYTES {
            push_issue(
                &mut issues,
                AdvisoryIssue {
                    source_file: Some(source_file),
                    state: AdvisoryState::Unavailable,
                    reason: AdvisoryIssueReason::OversizeFile,
                },
            );
            continue;
        }
        if total_bytes.saturating_add(bytes.len() as u64) > MAX_DISCOVERY_TOTAL_BYTES {
            truncated = true;
            push_issue(
                &mut issues,
                AdvisoryIssue {
                    source_file: None,
                    state: AdvisoryState::Unavailable,
                    reason: AdvisoryIssueReason::TotalByteLimit,
                },
            );
            break;
        }
        total_bytes = total_bytes.saturating_add(bytes.len() as u64);
        let content = match std::str::from_utf8(&bytes) {
            Ok(content) => content,
            Err(_) => {
                push_issue(
                    &mut issues,
                    AdvisoryIssue {
                        source_file: Some(source_file),
                        state: AdvisoryState::Unavailable,
                        reason: AdvisoryIssueReason::NonUtf8File,
                    },
                );
                continue;
            }
        };
        let Some(yaml) = frontmatter_yaml(content) else {
            continue;
        };
        let Ok(frontmatter) = serde_yaml::from_str::<ReferenceFrontmatter>(yaml) else {
            continue;
        };
        let Some(node) = frontmatter.rna else {
            continue;
        };
        for relationship in node.relationships {
            let Some(uri) = relationship.target.uri else {
                continue;
            };
            let Some(expected_kind) = OhReferenceKind::parse(relationship.target.kind.trim())
            else {
                continue;
            };
            if declarations.len() >= MAX_REFERENCE_DECLARATIONS {
                truncated = true;
                push_issue(
                    &mut issues,
                    AdvisoryIssue {
                        source_file: None,
                        state: AdvisoryState::Unavailable,
                        reason: AdvisoryIssueReason::DeclarationLimit,
                    },
                );
                break 'files;
            }
            declarations.push(ReferenceDeclaration {
                source_file: source_file.clone(),
                reference: uri.trim().to_string(),
                expected_kind,
            });
        }
    }
    declarations.sort_by(|left, right| {
        left.source_file
            .cmp(&right.source_file)
            .then_with(|| left.reference.cmp(&right.reference))
    });
    Ok(ReferenceDiscovery {
        declarations,
        issues,
        truncated,
    })
}

/// Appends a bounded discovery issue without allowing diagnostic floods.
fn push_issue(issues: &mut Vec<AdvisoryIssue>, issue: AdvisoryIssue) {
    if issues.len() < MAX_DISCOVERY_ISSUES {
        issues.push(issue);
    }
}

/// Extracts a leading YAML frontmatter block without parsing document bodies.
fn frontmatter_yaml(content: &str) -> Option<&str> {
    let trimmed = content.trim_start();
    let after = trimmed
        .strip_prefix("---")?
        .trim_start_matches(['\r', '\n']);
    let end = after.find("\n---")?;
    Some(&after[..end])
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};

    use tokio::sync::Notify;

    use super::*;

    #[derive(Debug, Clone)]
    struct FixtureTransport {
        response: Result<TransportResponse, String>,
        requests: Arc<Mutex<Vec<serde_json::Value>>>,
    }

    #[async_trait]
    impl ReferenceTransport for FixtureTransport {
        async fn resolve(
            &self,
            _endpoint: &str,
            _api_key: &str,
            reference: &OhReference,
            expected_kind: Option<OhReferenceKind>,
        ) -> Result<TransportResponse> {
            self.requests.lock().unwrap().push(serde_json::json!({
                "reference": reference.uri,
                "expected_kind": expected_kind,
            }));
            self.response.clone().map_err(anyhow::Error::msg)
        }
    }

    #[derive(Clone)]
    struct SlowTransport {
        delay: Duration,
        requests: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ReferenceTransport for SlowTransport {
        async fn resolve(
            &self,
            _endpoint: &str,
            _api_key: &str,
            reference: &OhReference,
            _expected_kind: Option<OhReferenceKind>,
        ) -> Result<TransportResponse> {
            self.requests.fetch_add(1, AtomicOrdering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Ok(TransportResponse {
                status: 200,
                body: serde_json::to_vec(&success(&reference.uri, "active", 1)).unwrap(),
            })
        }
    }

    #[derive(Clone)]
    struct BlockingTransport {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl ReferenceTransport for BlockingTransport {
        async fn resolve(
            &self,
            _endpoint: &str,
            _api_key: &str,
            reference: &OhReference,
            _expected_kind: Option<OhReferenceKind>,
        ) -> Result<TransportResponse> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(TransportResponse {
                status: 200,
                body: serde_json::to_vec(&success(&reference.uri, "active", 1)).unwrap(),
            })
        }
    }

    fn transport(status: u16, body: serde_json::Value) -> FixtureTransport {
        FixtureTransport {
            response: Ok(TransportResponse {
                status,
                body: serde_json::to_vec(&body).unwrap(),
            }),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn success(reference: &str, lifecycle: &str, version: u64) -> serde_json::Value {
        let parsed = OhReference::parse(reference).unwrap();
        serde_json::json!({
            "contract_version": "v1",
            "outcome": if lifecycle == "active" { "resolved" } else { "retired" },
            "reference": reference,
            "kind": parsed.kind,
            "lifecycle": lifecycle,
            "version": version,
        })
    }

    #[test]
    fn resolver_url_allows_ipv4_and_ipv6_loopback_http_only() {
        assert!(validated_resolver_url("http://localhost:8080/resolve").is_ok());
        assert!(validated_resolver_url("http://127.0.0.1:8080/resolve").is_ok());
        assert!(validated_resolver_url("http://[::1]:8080/resolve").is_ok());
        assert!(validated_resolver_url("http://192.0.2.1:8080/resolve").is_err());
        assert!(validated_resolver_url("http://[2001:db8::1]:8080/resolve").is_err());
        assert!(validated_resolver_url("https://resolver.example/resolve").is_ok());
    }

    fn resolver(
        temp: &tempfile::TempDir,
        transport: FixtureTransport,
    ) -> AdvisoryResolver<FixtureTransport> {
        resolver_with_authority(
            temp.path(),
            temp.path(),
            transport,
            "https://oh.example.test/api/references/resolve",
            "ak_fixture",
            Duration::from_secs(60),
        )
    }

    fn resolver_with_authority<T: ReferenceTransport>(
        repo: &Path,
        cache_base: &Path,
        transport: T,
        endpoint: &str,
        api_key: &str,
        ttl: Duration,
    ) -> AdvisoryResolver<T> {
        AdvisoryResolver::new_with_cache_base(
            transport,
            Some(endpoint.into()),
            Some(api_key.into()),
            repo,
            ttl,
            Some(cache_base),
        )
    }

    #[test]
    fn canonical_reference_matches_open_horizons_contract() {
        let parsed = OhReference::parse("oh://v1/endeavor/endeavor%3Adaily%3A1").unwrap();
        assert_eq!(parsed.id, "endeavor:daily:1");
        assert_eq!(parsed.kind, OhReferenceKind::Endeavor);
        for invalid in [
            "OH://v1/endeavor/id",
            "oh://V1/endeavor/id",
            "oh://v1/Endeavor/id",
            "oh://v1/endeavor/a/b",
            "oh://v1/endeavor/a%2fb",
            "oh://v1/endeavor/id?x=1",
            "oh://v1/endeavor/%69d",
        ] {
            assert!(OhReference::parse(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[tokio::test]
    async fn fixture_matrix_covers_rename_archive_delete_wrong_kind_and_access_failures() {
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/oh_reference_resolver/cases.json"
        ))
        .unwrap();
        for case in fixtures.as_array().unwrap() {
            let temp = tempfile::tempdir().unwrap();
            let reference = case["reference"].as_str().unwrap();
            let expected_state: AdvisoryState =
                serde_json::from_value(case["expected_state"].clone()).unwrap();
            let expected_kind = case["expected_kind"]
                .as_str()
                .and_then(OhReferenceKind::parse);
            let result = match case["mode"].as_str().unwrap_or("network") {
                "network" => {
                    let status = case["status"].as_u64().unwrap() as u16;
                    resolver(&temp, transport(status, case["body"].clone()))
                        .resolve_at(reference, expected_kind, false, 1_000)
                        .await
                }
                "offline_fresh" | "offline_stale" => {
                    resolver(&temp, transport(200, success(reference, "active", 9)))
                        .resolve_at(reference, expected_kind, false, 1_000)
                        .await;
                    let now = if case["mode"] == "offline_fresh" {
                        1_030
                    } else {
                        1_061
                    };
                    resolver(&temp, transport(503, serde_json::json!({})))
                        .resolve_at(reference, expected_kind, true, now)
                        .await
                }
                "transport_error" => {
                    let transport = FixtureTransport {
                        response: Err("endpoint failure".into()),
                        requests: Arc::new(Mutex::new(Vec::new())),
                    };
                    resolver(&temp, transport)
                        .resolve_at(reference, expected_kind, false, 1_000)
                        .await
                }
                mode => panic!("unsupported fixture mode {mode}"),
            };
            assert_eq!(result.state, expected_state, "fixture {}", case["name"]);
        }
    }

    #[tokio::test]
    async fn cache_is_minimal_redacted_and_offline_capable() {
        let temp = tempfile::tempdir().unwrap();
        let reference = "oh://v1/context/context%3Ashared%3A1";
        let transport = transport(200, success(reference, "active", 7));
        let requests = transport.requests.clone();
        let resolver = resolver(&temp, transport);
        let online = resolver
            .resolve_at(reference, Some(OhReferenceKind::Context), false, 1_000)
            .await;
        assert_eq!(online.state, AdvisoryState::Confirmed);
        assert_eq!(online.source, ResolutionSource::Network);

        let cache_text = fs::read_to_string(resolver.cache_path.as_ref().unwrap()).unwrap();
        for forbidden in [
            "nodes",
            "edges",
            "body",
            "source",
            "embedding",
            "outbox",
            "api_key",
        ] {
            assert!(!cache_text.contains(forbidden), "cache leaked {forbidden}");
        }
        let request = requests.lock().unwrap().first().cloned().unwrap();
        assert_eq!(
            request
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["expected_kind", "reference"]
        );

        let offline = resolver
            .resolve_at(reference, Some(OhReferenceKind::Context), true, 1_030)
            .await;
        assert_eq!(offline.state, AdvisoryState::Confirmed);
        assert_eq!(offline.source, ResolutionSource::Cache);
        let clock_rollback = resolver
            .resolve_at(reference, Some(OhReferenceKind::Context), true, 999)
            .await;
        assert_eq!(clock_rollback.state, AdvisoryState::Stale);
        let stale = resolver
            .resolve_at(reference, Some(OhReferenceKind::Context), true, 1_061)
            .await;
        assert_eq!(stale.state, AdvisoryState::Stale);
        assert_eq!(stale.version, Some(7));
    }

    #[tokio::test]
    async fn cache_ttl_is_hard_capped_independently_of_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let reference = "oh://v1/context/context-ttl";
        let resolver = resolver_with_authority(
            temp.path(),
            temp.path(),
            transport(200, success(reference, "active", 1)),
            "https://oh.example.test/api/references/resolve",
            "ak_fixture",
            Duration::from_secs(u64::MAX),
        );
        resolver
            .resolve_at(reference, Some(OhReferenceKind::Context), false, 1_000)
            .await;
        let result = resolver
            .resolve_at(
                reference,
                Some(OhReferenceKind::Context),
                true,
                1_000 + MAX_CACHE_TTL_SECONDS + 1,
            )
            .await;
        assert_eq!(result.state, AdvisoryState::Stale);
    }

    #[tokio::test]
    async fn authority_and_credential_namespaces_are_isolated_offline() {
        let repo = tempfile::tempdir().unwrap();
        let cache_base = tempfile::tempdir().unwrap();
        let reference = "oh://v1/context/context-authority";
        let primary = resolver_with_authority(
            repo.path(),
            cache_base.path(),
            transport(200, success(reference, "active", 1)),
            "https://oh.example.test/api/references/resolve/",
            "ak_primary_high_entropy",
            Duration::from_secs(60),
        );
        assert!(
            !primary
                .cache_path
                .as_ref()
                .unwrap()
                .to_string_lossy()
                .contains("ak_primary_high_entropy")
        );
        primary
            .resolve_at(reference, Some(OhReferenceKind::Context), false, 1_000)
            .await;

        let equivalent_endpoint = resolver_with_authority(
            repo.path(),
            cache_base.path(),
            transport(503, serde_json::json!({})),
            "https://OH.EXAMPLE.TEST/api/references/resolve",
            "ak_primary_high_entropy",
            Duration::from_secs(60),
        )
        .resolve_at(reference, Some(OhReferenceKind::Context), true, 1_001)
        .await;
        assert_eq!(equivalent_endpoint.state, AdvisoryState::Confirmed);

        let other_key = resolver_with_authority(
            repo.path(),
            cache_base.path(),
            transport(503, serde_json::json!({})),
            "https://oh.example.test/api/references/resolve",
            "ak_other_high_entropy",
            Duration::from_secs(60),
        )
        .resolve_at(reference, Some(OhReferenceKind::Context), true, 1_001)
        .await;
        assert_eq!(other_key.state, AdvisoryState::Unavailable);

        let other_endpoint = resolver_with_authority(
            repo.path(),
            cache_base.path(),
            transport(503, serde_json::json!({})),
            "https://other.example.test/api/references/resolve",
            "ak_primary_high_entropy",
            Duration::from_secs(60),
        )
        .resolve_at(reference, Some(OhReferenceKind::Context), true, 1_001)
        .await;
        assert_eq!(other_endpoint.state, AdvisoryState::Unavailable);

        let no_authority = AdvisoryResolver::new_with_cache_base(
            transport(503, serde_json::json!({})),
            None,
            None,
            repo.path(),
            Duration::from_secs(60),
            Some(cache_base.path()),
        )
        .resolve_at(reference, Some(OhReferenceKind::Context), true, 1_001)
        .await;
        assert_eq!(no_authority.state, AdvisoryState::Unavailable);
    }

    #[tokio::test]
    async fn tracked_legacy_preseed_is_never_selected_as_authorized_cache() {
        let repo_dir = tempfile::tempdir().unwrap();
        let cache_base = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(repo_dir.path()).unwrap();
        let legacy = repo_dir.path().join(LEGACY_CACHE_RELATIVE_PATH);
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &legacy,
            r#"{"schema_version":1,"entries":{"oh://v1/context/context-preseed":{"reference":"oh://v1/context/context-preseed","kind":"context","lifecycle":"active","version":1,"checked_at_unix_seconds":1000}}}"#,
        )
        .unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_path(Path::new(LEGACY_CACHE_RELATIVE_PATH))
            .unwrap();
        index.write().unwrap();

        let result = resolver_with_authority(
            repo_dir.path(),
            cache_base.path(),
            transport(503, serde_json::json!({})),
            "https://oh.example.test/api/references/resolve",
            "ak_fixture",
            Duration::from_secs(60),
        )
        .resolve_at(
            "oh://v1/context/context-preseed",
            Some(OhReferenceKind::Context),
            true,
            1_001,
        )
        .await;
        assert_eq!(result.state, AdvisoryState::Unavailable);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn permissive_or_symlink_cache_is_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = tempfile::tempdir().unwrap();
        let reference = "oh://v1/context/context-mode";
        let resolver = resolver(&temp, transport(200, success(reference, "active", 1)));
        resolver
            .resolve_at(reference, Some(OhReferenceKind::Context), false, 1_000)
            .await;
        let cache_path = resolver.cache_path.as_ref().unwrap();
        assert_eq!(
            fs::metadata(cache_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::set_permissions(cache_path, fs::Permissions::from_mode(0o644)).unwrap();
        let permissive = resolver
            .resolve_at(reference, Some(OhReferenceKind::Context), true, 1_001)
            .await;
        assert_eq!(permissive.state, AdvisoryState::Unavailable);

        fs::remove_file(cache_path).unwrap();
        let target = temp.path().join("attacker-cache.json");
        fs::write(&target, b"{}").unwrap();
        symlink(&target, cache_path).unwrap();
        let symlinked = resolver
            .resolve_at(reference, Some(OhReferenceKind::Context), true, 1_001)
            .await;
        assert_eq!(symlinked.state, AdvisoryState::Unavailable);
    }

    #[cfg(unix)]
    #[test]
    fn unix_cache_owner_and_mode_validation_is_fail_closed() {
        let uid = current_effective_uid();
        assert!(validate_unix_owner_and_mode(uid, 0o100600, uid).is_ok());
        assert!(validate_unix_owner_and_mode(uid, 0o100640, uid).is_err());
        assert!(validate_unix_owner_and_mode(uid.saturating_add(1), 0o100600, uid).is_err());
    }

    #[test]
    fn ten_thousand_and_first_cache_entry_is_pruned_and_remains_loadable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("bounded-cache.json");
        let mut cache = ResolutionCache::default();
        for index in 0..=MAX_CACHE_ENTRIES {
            let reference = format!("oh://v1/context/context-{index:05}");
            cache.entries.insert(
                reference.clone(),
                CachedResolution {
                    reference,
                    kind: OhReferenceKind::Context,
                    lifecycle: OhReferenceLifecycle::Active,
                    version: 1,
                    checked_at_unix_seconds: index as u64,
                },
            );
        }
        cache.persist(&path).unwrap();
        let loaded = ResolutionCache::load(&path).unwrap();
        assert_eq!(loaded.entries.len(), MAX_CACHE_ENTRIES);
        assert!(!loaded.entries.contains_key("oh://v1/context/context-00000"));
        assert!(loaded.entries.contains_key("oh://v1/context/context-10000"));
    }

    #[tokio::test]
    async fn endpoint_failure_is_unavailable_without_cache_and_stale_with_expired_cache() {
        let temp = tempfile::tempdir().unwrap();
        let reference = "oh://v1/metis/metis-1";
        let seed = resolver(&temp, transport(200, success(reference, "active", 2)));
        seed.resolve_at(reference, Some(OhReferenceKind::Metis), false, 1_000)
            .await;
        let failure = FixtureTransport {
            response: Err("endpoint failure".into()),
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        let stale = resolver(&temp, failure.clone())
            .resolve_at(reference, Some(OhReferenceKind::Metis), false, 1_100)
            .await;
        assert_eq!(stale.state, AdvisoryState::Stale);
        let empty = tempfile::tempdir().unwrap();
        let unavailable = resolver(&empty, failure)
            .resolve_at(reference, Some(OhReferenceKind::Metis), false, 1_100)
            .await;
        assert_eq!(unavailable.state, AdvisoryState::Unavailable);
    }

    #[tokio::test]
    async fn authorization_failure_evicts_prior_authorized_cache() {
        let temp = tempfile::tempdir().unwrap();
        let reference = "oh://v1/guardrail/guardrail-1";
        resolver(&temp, transport(200, success(reference, "active", 2)))
            .resolve_at(reference, Some(OhReferenceKind::Guardrail), false, 1_000)
            .await;

        let denied = resolver(
            &temp,
            transport(401, serde_json::json!({ "error": "invalid_api_key" })),
        )
        .resolve_at(reference, Some(OhReferenceKind::Guardrail), false, 1_001)
        .await;
        assert_eq!(denied.state, AdvisoryState::Unauthorized);

        let offline = resolver(&temp, transport(503, serde_json::json!({})))
            .resolve_at(reference, Some(OhReferenceKind::Guardrail), true, 1_002)
            .await;
        assert_eq!(offline.state, AdvisoryState::Unavailable);
        assert_eq!(offline.source, ResolutionSource::None);
    }

    #[tokio::test]
    async fn online_lock_serializes_delayed_success_before_authorization_eviction() {
        let repo = tempfile::tempdir().unwrap();
        let cache_base = tempfile::tempdir().unwrap();
        let reference = "oh://v1/guardrail/guardrail-race";
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let delayed = resolver_with_authority(
            repo.path(),
            cache_base.path(),
            BlockingTransport {
                started: started.clone(),
                release: release.clone(),
            },
            "https://oh.example.test/api/references/resolve",
            "ak_fixture",
            Duration::from_secs(60),
        );
        let denied = resolver_with_authority(
            repo.path(),
            cache_base.path(),
            transport(401, serde_json::json!({ "error": "invalid_api_key" })),
            "https://oh.example.test/api/references/resolve",
            "ak_fixture",
            Duration::from_secs(60),
        );
        let success_task = tokio::spawn(async move {
            delayed
                .resolve_at(reference, Some(OhReferenceKind::Guardrail), false, 1_000)
                .await
        });
        started.notified().await;
        let denial_task = tokio::spawn(async move {
            denied
                .resolve_at(reference, Some(OhReferenceKind::Guardrail), false, 1_001)
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !denial_task.is_finished(),
            "401 request bypassed cache lock"
        );
        release.notify_one();
        assert_eq!(success_task.await.unwrap().state, AdvisoryState::Confirmed);
        assert_eq!(
            denial_task.await.unwrap().state,
            AdvisoryState::Unauthorized
        );

        let offline = resolver_with_authority(
            repo.path(),
            cache_base.path(),
            transport(503, serde_json::json!({})),
            "https://oh.example.test/api/references/resolve",
            "ak_fixture",
            Duration::from_secs(60),
        )
        .resolve_at(reference, Some(OhReferenceKind::Guardrail), true, 1_002)
        .await;
        assert_eq!(offline.state, AdvisoryState::Unavailable);
    }

    #[tokio::test]
    async fn explicit_batches_are_count_and_wall_clock_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let resolver = resolver_with_authority(
            temp.path(),
            temp.path(),
            SlowTransport {
                delay: Duration::from_millis(100),
                requests: requests.clone(),
            },
            "https://oh.example.test/api/references/resolve",
            "ak_fixture",
            Duration::from_secs(60),
        );
        let declarations = (0..(MAX_REFERENCE_DECLARATIONS + 10))
            .map(|index| ReferenceDeclaration {
                source_file: PathBuf::new(),
                reference: format!("oh://v1/context/context-{index}"),
                expected_kind: OhReferenceKind::Context,
            })
            .collect();
        let started = tokio::time::Instant::now();
        let batch = resolve_declarations_with_deadline(
            &resolver,
            declarations,
            None,
            false,
            Vec::new(),
            false,
            Duration::from_millis(25),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(batch.resolutions.len(), MAX_REFERENCE_DECLARATIONS);
        assert!(batch.truncated);
        assert!(
            batch
                .issues
                .iter()
                .any(|issue| issue.reason == AdvisoryIssueReason::DeclarationLimit)
        );
        assert!(
            batch
                .issues
                .iter()
                .any(|issue| issue.reason == AdvisoryIssueReason::BatchDeadline)
        );
        assert_eq!(requests.load(AtomicOrdering::SeqCst), 1);
    }

    #[tokio::test]
    async fn oversized_explicit_reference_is_not_echoed_into_output() {
        let temp = tempfile::tempdir().unwrap();
        let oversized = "x".repeat(MAX_REFERENCE_INPUT_BYTES + 1);
        let batch = resolve_declarations(
            &resolver(&temp, transport(503, serde_json::json!({}))),
            preflight_explicit_references(vec![oversized.clone()], None),
            None,
            true,
        )
        .await;
        let json = serde_json::to_string(&batch).unwrap();
        assert!(!json.contains(&oversized));
        assert!(json.contains(OVERSIZE_REFERENCE_MARKER));
        assert!(json.len() < 1_024);
    }

    #[test]
    fn explicit_preflight_drops_excess_count_and_raw_capacity_before_parsing() {
        let secret = "secret-raw-reference".repeat(MAX_REFERENCE_INPUT_BYTES / 4 + 1);
        let mut references = Vec::with_capacity(MAX_REFERENCE_DECLARATIONS * 100);
        references.push(secret.clone());
        references.extend(
            (1..(MAX_REFERENCE_DECLARATIONS + 100))
                .map(|index| format!("oh://v1/context/context-{index}")),
        );

        let discovery = preflight_explicit_references(references, None);
        assert_eq!(discovery.declarations.len(), MAX_REFERENCE_DECLARATIONS);
        assert!(discovery.declarations.capacity() <= MAX_REFERENCE_DECLARATIONS);
        assert_eq!(
            discovery.declarations[0].reference,
            OVERSIZE_REFERENCE_MARKER
        );
        assert!(discovery.declarations[0].reference.capacity() <= OVERSIZE_REFERENCE_MARKER.len());
        assert!(discovery.truncated);
        assert!(
            discovery
                .issues
                .iter()
                .any(|issue| issue.reason == AdvisoryIssueReason::DeclarationLimit)
        );
        assert!(
            discovery
                .issues
                .iter()
                .any(|issue| issue.reason == AdvisoryIssueReason::OversizeReference)
        );
        let json = serde_json::to_string(&discovery).unwrap();
        assert!(!json.contains(&secret));
        assert!(json.len() < 64 * 1024);
    }

    #[test]
    fn declaration_discovery_preserves_local_targets_and_finds_only_oh_uris() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("knowledge.md"),
            r#"---
rna:
  kind: claim
  id: claim-1
  relationships:
    - kind: supports
      target:
        kind: claim
        id: local-claim
        file: local.md
    - kind: informs
      target:
        kind: endeavor
        uri: "  oh://v1/endeavor/endeavor-1  "
---
# Knowledge
"#,
        )
        .unwrap();
        let discovery = collect_reference_declarations(temp.path()).unwrap();
        assert_eq!(discovery.declarations.len(), 1);
        assert_eq!(
            discovery.declarations[0].reference,
            "oh://v1/endeavor/endeavor-1"
        );
        assert_eq!(
            discovery.declarations[0].expected_kind,
            OhReferenceKind::Endeavor
        );
    }

    #[test]
    fn discovery_skips_and_reports_non_utf8_and_oversize_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("good.md"),
            "---\nrna:\n  relationships:\n    - target:\n        kind: context\n        uri: oh://v1/context/context-good\n---\n",
        )
        .unwrap();
        fs::write(temp.path().join("non-utf8.md"), [0xff, 0xfe]).unwrap();
        fs::write(
            temp.path().join("oversize.md"),
            vec![b'x'; MAX_DISCOVERY_FILE_BYTES as usize + 1],
        )
        .unwrap();

        let discovery = collect_reference_declarations(temp.path()).unwrap();
        assert_eq!(discovery.declarations.len(), 1);
        assert!(
            discovery
                .issues
                .iter()
                .any(|issue| issue.reason == AdvisoryIssueReason::NonUtf8File)
        );
        assert!(
            discovery
                .issues
                .iter()
                .any(|issue| issue.reason == AdvisoryIssueReason::OversizeFile)
        );
    }

    #[cfg(unix)]
    #[test]
    fn tracked_directory_symlink_cannot_escape_repo_discovery() {
        use std::os::unix::fs::symlink;

        let repo_dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(
            outside.path().join("external.md"),
            "---\nrna:\n  relationships:\n    - target:\n        kind: context\n        uri: oh://v1/context/outside-secret\n---\n",
        )
        .unwrap();
        let repo = git2::Repository::init(repo_dir.path()).unwrap();
        symlink(outside.path(), repo_dir.path().join("docs")).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("docs")).unwrap();
        index.write().unwrap();

        let discovery = collect_reference_declarations(repo_dir.path()).unwrap();
        assert!(discovery.declarations.is_empty());
        assert!(discovery.issues.is_empty());
        assert!(!discovery.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_skips_and_reports_unreadable_markdown() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let unreadable = temp.path().join("unreadable.md");
        fs::write(&unreadable, "secret").unwrap();
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
        let discovery = collect_reference_declarations(temp.path()).unwrap();
        assert!(
            discovery
                .issues
                .iter()
                .any(|issue| issue.reason == AdvisoryIssueReason::UnreadableFile)
        );
    }

    #[test]
    fn discovery_caps_declarations_and_reports_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let mut markdown = String::from("---\nrna:\n  relationships:\n");
        for index in 0..(MAX_REFERENCE_DECLARATIONS + 1) {
            markdown.push_str(&format!(
                "    - target:\n        kind: context\n        uri: oh://v1/context/context-{index}\n"
            ));
        }
        markdown.push_str("---\n");
        fs::write(temp.path().join("many.md"), markdown).unwrap();
        let discovery = collect_reference_declarations(temp.path()).unwrap();
        assert_eq!(discovery.declarations.len(), MAX_REFERENCE_DECLARATIONS);
        assert!(discovery.truncated);
        assert!(
            discovery
                .issues
                .iter()
                .any(|issue| issue.reason == AdvisoryIssueReason::DeclarationLimit)
        );
    }

    #[tokio::test]
    async fn shared_service_resolves_duplicate_identity_once_and_preserves_sources() {
        let temp = tempfile::tempdir().unwrap();
        let reference = "oh://v1/context/context-1";
        let transport = transport(200, success(reference, "active", 1));
        let requests = transport.requests.clone();
        let resolver = resolver(&temp, transport);
        let output = resolve_declarations(
            &resolver,
            ReferenceDiscovery {
                declarations: vec![
                    ReferenceDeclaration {
                        source_file: PathBuf::from("a.md"),
                        reference: reference.into(),
                        expected_kind: OhReferenceKind::Context,
                    },
                    ReferenceDeclaration {
                        source_file: PathBuf::from("b.md"),
                        reference: reference.into(),
                        expected_kind: OhReferenceKind::Context,
                    },
                ],
                ..ReferenceDiscovery::default()
            },
            None,
            false,
        )
        .await;
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert_eq!(output.resolutions.len(), 2);
        assert_eq!(
            output.resolutions[0].source_file,
            Some(PathBuf::from("a.md"))
        );
        assert_eq!(
            output.resolutions[1].source_file,
            Some(PathBuf::from("b.md"))
        );
    }
}
