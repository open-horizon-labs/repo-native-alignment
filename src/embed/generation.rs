use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const GENERATION_SCHEMA_VERSION: u32 = 2;
pub const COVERAGE_SCHEMA_VERSION: u32 = 2;
pub const CURRENT_POINTER_SCHEMA_VERSION: u32 = 2;
pub const VERIFICATION_SCHEMA_VERSION: u32 = 2;
pub const SEMANTIC_SCHEMA_SIGNATURE: &str = "rna.embedding-generation.v2:id-kind-title-body-text_hash-file_path-language-subsystem-cyclomatic-vector-f32:value-addressed-vector-input";
pub const PREPROCESSING_VERSION: &str =
    "rna-minilm-preprocessing-v2-stable-semantic-metadata-char-budget-650";
pub const TOKENIZER_IDENTITY: &str =
    "sentence-transformers/all-MiniLM-L6-v2:metal-candle-tokenizer-v1";

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticIdentity {
    pub model: String,
    pub tokenizer: String,
    pub model_files_digest: String,
    pub model_sha256: String,
    pub tokenizer_sha256: String,
    pub reranker_files_digest: String,
    pub preprocessing_version: String,
    pub artifact_sha256: String,
    pub schema_signature: String,
    pub dimension: usize,
    pub flags: BTreeMap<String, String>,
}

impl SemanticIdentity {
    pub fn for_current_process(
        dimension: usize,
        mut flags: BTreeMap<String, String>,
    ) -> Result<Self> {
        flags.insert(
            "embeddings_feature".to_string(),
            cfg!(feature = "embeddings").to_string(),
        );
        flags.insert(
            "metal_feature".to_string(),
            cfg!(feature = "metal").to_string(),
        );
        let (model_files_digest, model_sha256, tokenizer_sha256, reranker_files_digest) =
            runtime_asset_digests()?;
        Ok(Self {
            model: crate::embed::EMBEDDING_MODEL_NAME.to_string(),
            tokenizer: TOKENIZER_IDENTITY.to_string(),
            model_files_digest,
            model_sha256,
            tokenizer_sha256,
            reranker_files_digest,
            preprocessing_version: PREPROCESSING_VERSION.to_string(),
            artifact_sha256: current_executable_sha256()?,
            schema_signature: SEMANTIC_SCHEMA_SIGNATURE.to_string(),
            dimension,
            flags,
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.model.trim().is_empty()
            || self.tokenizer.trim().is_empty()
            || self.preprocessing_version.trim().is_empty()
            || self.schema_signature.trim().is_empty()
        {
            bail!("semantic identity contains an empty required field");
        }
        require_sha256("semantic identity artifact_sha256", &self.artifact_sha256)?;
        for (name, value) in [
            (
                "semantic identity model_files_digest",
                &self.model_files_digest,
            ),
            ("semantic identity model_sha256", &self.model_sha256),
            ("semantic identity tokenizer_sha256", &self.tokenizer_sha256),
            (
                "semantic identity reranker_files_digest",
                &self.reranker_files_digest,
            ),
        ] {
            require_sha256(name, value)?;
        }
        if self.dimension == 0 {
            bail!("semantic identity dimension must be greater than zero");
        }
        if self.schema_signature != SEMANTIC_SCHEMA_SIGNATURE {
            bail!(
                "semantic schema signature mismatch: expected {}, received {} (rebuild; migrations are forbidden)",
                SEMANTIC_SCHEMA_SIGNATURE,
                self.schema_signature
            );
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String> {
        self.validate()?;
        Ok(sha256_bytes(&canonical_json_bytes(self)?))
    }
}

pub fn runtime_asset_digests() -> Result<(String, String, String, String)> {
    fn value(name: &str, fallback_label: &str) -> Result<String> {
        match std::env::var(name) {
            Ok(value) => {
                require_sha256(name, &value)?;
                Ok(value)
            }
            Err(_) => Ok(sha256_bytes(fallback_label.as_bytes())),
        }
    }

    Ok((
        value(
            "RNA_EMBEDDING_MODEL_FILES_DIGEST",
            "unqualified:MiniLM-L6-v2:model-files",
        )?,
        value(
            "RNA_EMBEDDING_MODEL_SHA256",
            "unqualified:MiniLM-L6-v2:model.safetensors",
        )?,
        value(
            "RNA_EMBEDDING_TOKENIZER_SHA256",
            "unqualified:MiniLM-L6-v2:tokenizer.json",
        )?,
        value(
            "RNA_RERANKER_MODEL_FILES_DIGEST",
            "unqualified:Jina-Reranker-v1-Turbo:model-files",
        )?,
    ))
}

/// Verify that the encoder cache selected at runtime matches the configured
/// asset digests. Environment digests are trust anchors, so the actual HF_HOME
/// tree and uniquely named snapshot assets must match before use.
pub fn verify_runtime_encoder_assets() -> Result<(String, String, String)> {
    let (expected_tree, expected_model, expected_tokenizer, _) = runtime_asset_digests()?;
    let root = std::env::var("HF_HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("runtime asset verification requires HF_HOME"))?;
    let entries = verified_tree_entries(&root)?;
    let observed_tree = sha256_bytes(&canonical_json_bytes(&entries)?);
    if observed_tree != expected_tree {
        bail!("encoder cache tree digest does not match the configured asset identity");
    }

    let unique_snapshot_asset = |name: &str| -> Result<&TreeEntry> {
        let matches = entries
            .iter()
            .filter(|entry| {
                entry.path.split('/').any(|part| part == "snapshots")
                    && entry.path.rsplit('/').next() == Some(name)
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            bail!(
                "encoder cache requires exactly one snapshot asset named {name}; observed {}",
                matches.len()
            );
        }
        Ok(matches[0])
    };
    let model = unique_snapshot_asset("model.safetensors")?;
    let tokenizer = unique_snapshot_asset("tokenizer.json")?;
    if model.sha256 != expected_model {
        bail!("encoder model.safetensors digest does not match the configured asset identity");
    }
    if tokenizer.sha256 != expected_tokenizer {
        bail!("encoder tokenizer.json digest does not match the configured asset identity");
    }
    if entries.iter().any(|entry| {
        entry.path.split('/').any(|part| part == "snapshots")
            && entry.path.rsplit('/').next() == Some("pytorch_model.bin")
    }) {
        bail!("encoder PyTorch weight fallback is forbidden");
    }
    Ok((
        observed_tree,
        model.sha256.clone(),
        tokenizer.sha256.clone(),
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeviceAttestation {
    pub required_device: String,
    pub observed_device: String,
    pub backend: String,
    pub device_index: Option<usize>,
    pub artifact_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticBuildContract {
    pub structural_graph_snapshot_digest: String,
    pub flags: BTreeMap<String, String>,
    pub require_metal: bool,
}

impl SemanticBuildContract {
    pub fn strict(
        structural_graph_snapshot_digest: String,
        flags: BTreeMap<String, String>,
    ) -> Result<Self> {
        require_sha256(
            "structural graph snapshot digest",
            &structural_graph_snapshot_digest,
        )?;
        Ok(Self {
            structural_graph_snapshot_digest,
            flags,
            require_metal: true,
        })
    }

    pub fn validate(&self) -> Result<()> {
        require_sha256(
            "structural graph snapshot digest",
            &self.structural_graph_snapshot_digest,
        )
    }
}

impl DeviceAttestation {
    pub fn validate(&self, identity: &SemanticIdentity) -> Result<()> {
        require_sha256("device attestation artifact_sha256", &self.artifact_sha256)?;
        if self.artifact_sha256 != identity.artifact_sha256 {
            bail!("device attestation artifact identity does not match semantic identity");
        }
        if self.required_device == "metal"
            && (self.observed_device != "metal" || self.backend != "candle-metal")
        {
            bail!(
                "strict semantic generation requires candle Metal, observed device={} backend={}",
                self.observed_device,
                self.backend
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct CoverageRow {
    pub id: String,
    pub canonical_input_digest: String,
    pub vector_sha256: String,
}

impl CoverageRow {
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() {
            bail!("semantic coverage row id must not be empty");
        }
        require_sha256(
            "semantic coverage canonical_input_digest",
            &self.canonical_input_digest,
        )?;
        require_sha256("semantic coverage vector_sha256", &self.vector_sha256)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoverageManifest {
    pub schema_version: u32,
    pub generation_digest: String,
    pub semantic_identity_digest: String,
    pub canonical_input_digest: String,
    pub target_graph_digest: String,
    pub structural_graph_snapshot_digest: String,
    pub row_count: usize,
    pub rows: Vec<CoverageRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorReusePlan {
    pub reused_ids: Vec<String>,
    pub encode_ids: Vec<String>,
}

pub fn plan_vector_reuse(
    prior_identity: Option<&SemanticIdentity>,
    target_identity: &SemanticIdentity,
    prior_rows: &[CoverageRow],
    target_rows: &[(String, String)],
) -> Result<VectorReusePlan> {
    target_identity.validate()?;
    let mut target = target_rows.to_vec();
    target.sort();
    for pair in target.windows(2) {
        if pair[0].0 == pair[1].0 {
            bail!("target semantic inputs contain duplicate id {}", pair[0].0);
        }
    }
    for (_, digest) in &target {
        require_sha256("target semantic input digest", digest)?;
    }
    if prior_identity != Some(target_identity) {
        return Ok(VectorReusePlan {
            reused_ids: Vec::new(),
            encode_ids: target.into_iter().map(|(id, _)| id).collect(),
        });
    }
    let mut prior_ids = BTreeSet::new();
    let mut prior_inputs = BTreeSet::new();
    for row in prior_rows {
        row.validate()?;
        if !prior_ids.insert(row.id.as_str()) {
            bail!("prior semantic coverage contains duplicate id {}", row.id);
        }
        prior_inputs.insert(row.canonical_input_digest.as_str());
    }
    let mut reused_ids = Vec::new();
    let mut encode_ids = Vec::new();
    for (id, digest) in target {
        if prior_inputs.contains(digest.as_str()) {
            reused_ids.push(id);
        } else {
            encode_ids.push(id);
        }
    }
    Ok(VectorReusePlan {
        reused_ids,
        encode_ids,
    })
}

impl CoverageManifest {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != COVERAGE_SCHEMA_VERSION {
            bail!(
                "semantic coverage schema mismatch: expected {}, received {} (rebuild; migrations are forbidden)",
                COVERAGE_SCHEMA_VERSION,
                self.schema_version
            );
        }
        for (name, value) in [
            ("generation_digest", &self.generation_digest),
            ("semantic_identity_digest", &self.semantic_identity_digest),
            ("canonical_input_digest", &self.canonical_input_digest),
            ("target_graph_digest", &self.target_graph_digest),
            (
                "structural_graph_snapshot_digest",
                &self.structural_graph_snapshot_digest,
            ),
        ] {
            require_sha256(name, value)?;
        }
        if self.row_count != self.rows.len() {
            bail!(
                "semantic coverage row_count mismatch: declared {}, observed {}",
                self.row_count,
                self.rows.len()
            );
        }
        let mut previous: Option<&str> = None;
        for row in &self.rows {
            row.validate()?;
            if previous.is_some_and(|id| id >= row.id.as_str()) {
                bail!("semantic coverage rows must be uniquely sorted by id");
            }
            previous = Some(&row.id);
        }
        let aggregate = canonical_input_digest(
            self.rows
                .iter()
                .map(|row| (row.id.clone(), row.canonical_input_digest.clone())),
        )?;
        if aggregate != self.canonical_input_digest {
            bail!("semantic coverage canonical input digest mismatch");
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String> {
        self.validate()?;
        Ok(sha256_bytes(&canonical_json_bytes(self)?))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationManifest {
    pub schema_version: u32,
    pub generation_digest: String,
    pub semantic_identity: SemanticIdentity,
    pub semantic_identity_digest: String,
    pub canonical_input_digest: String,
    pub target_graph_digest: String,
    pub structural_graph_snapshot_digest: String,
    pub row_count: usize,
    pub coverage_digest: String,
    pub lance_tree_digest: String,
    pub reused_vector_count: usize,
    pub encoded_vector_count: usize,
    pub prior_generation_digest: Option<String>,
    pub created_by_artifact_sha256: String,
    pub device_attestation: DeviceAttestation,
}

impl GenerationManifest {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != GENERATION_SCHEMA_VERSION {
            bail!(
                "semantic generation schema mismatch: expected {}, received {} (rebuild; migrations are forbidden)",
                GENERATION_SCHEMA_VERSION,
                self.schema_version
            );
        }
        self.semantic_identity.validate()?;
        self.device_attestation.validate(&self.semantic_identity)?;
        for (name, value) in [
            ("generation_digest", &self.generation_digest),
            ("semantic_identity_digest", &self.semantic_identity_digest),
            ("canonical_input_digest", &self.canonical_input_digest),
            ("target_graph_digest", &self.target_graph_digest),
            (
                "structural_graph_snapshot_digest",
                &self.structural_graph_snapshot_digest,
            ),
            ("coverage_digest", &self.coverage_digest),
            ("lance_tree_digest", &self.lance_tree_digest),
            (
                "created_by_artifact_sha256",
                &self.created_by_artifact_sha256,
            ),
        ] {
            require_sha256(name, value)?;
        }
        if let Some(prior) = &self.prior_generation_digest {
            require_sha256("prior_generation_digest", prior)?;
        }
        if self.semantic_identity.digest()? != self.semantic_identity_digest {
            bail!("semantic identity digest mismatch");
        }
        if self.semantic_identity.artifact_sha256 != self.created_by_artifact_sha256 {
            bail!("semantic generation producer artifact mismatch");
        }
        if generation_digest(
            &self.semantic_identity,
            &self.canonical_input_digest,
            &self.target_graph_digest,
            &self.structural_graph_snapshot_digest,
        )? != self.generation_digest
        {
            bail!("semantic generation digest mismatch");
        }
        if self.reused_vector_count + self.encoded_vector_count != self.row_count {
            bail!("semantic generation reuse/encode counts do not cover every row");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurrentPointer {
    pub schema_version: u32,
    pub generation_digest: String,
    pub manifest_sha256: String,
    pub verification_sha256: String,
}

impl CurrentPointer {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CURRENT_POINTER_SCHEMA_VERSION {
            bail!(
                "semantic current pointer schema mismatch: expected {}, received {}",
                CURRENT_POINTER_SCHEMA_VERSION,
                self.schema_version
            );
        }
        require_sha256("current generation_digest", &self.generation_digest)?;
        require_sha256("current manifest_sha256", &self.manifest_sha256)?;
        require_sha256("current verification_sha256", &self.verification_sha256)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticVerificationReceipt {
    pub schema_version: u32,
    pub generation_digest: String,
    pub manifest_sha256: String,
    pub coverage_digest: String,
    pub lance_tree_digest: String,
    pub structural_graph_snapshot_digest: String,
    pub target_graph_digest: String,
    pub row_count: usize,
    pub one_to_one_coverage: bool,
    pub fresh_reopen_ready: bool,
}

impl SemanticVerificationReceipt {
    pub fn validate_against(&self, manifest: &GenerationManifest) -> Result<()> {
        if self.schema_version != VERIFICATION_SCHEMA_VERSION {
            bail!(
                "semantic verification schema mismatch: expected {}, received {}",
                VERIFICATION_SCHEMA_VERSION,
                self.schema_version
            );
        }
        for (name, value) in [
            ("verification generation_digest", &self.generation_digest),
            ("verification manifest_sha256", &self.manifest_sha256),
            ("verification coverage_digest", &self.coverage_digest),
            ("verification lance_tree_digest", &self.lance_tree_digest),
            (
                "verification structural_graph_snapshot_digest",
                &self.structural_graph_snapshot_digest,
            ),
            (
                "verification target_graph_digest",
                &self.target_graph_digest,
            ),
        ] {
            require_sha256(name, value)?;
        }
        if !self.one_to_one_coverage || !self.fresh_reopen_ready {
            bail!("semantic verification receipt is not READY");
        }
        if self.generation_digest != manifest.generation_digest
            || self.coverage_digest != manifest.coverage_digest
            || self.lance_tree_digest != manifest.lance_tree_digest
            || self.structural_graph_snapshot_digest != manifest.structural_graph_snapshot_digest
            || self.target_graph_digest != manifest.target_graph_digest
            || self.row_count != manifest.row_count
        {
            bail!("semantic verification receipt does not match generation manifest");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
struct GenerationKey<'a> {
    schema_version: u32,
    semantic_identity: &'a SemanticIdentity,
    canonical_input_digest: &'a str,
    target_graph_digest: &'a str,
    structural_graph_snapshot_digest: &'a str,
}

#[derive(Debug, Clone, Serialize)]
struct CanonicalInputRow<'a> {
    id: &'a str,
    canonical_input_digest: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
struct TreeEntry {
    path: String,
    size: u64,
    sha256: String,
}

pub fn semantic_root(repo_root: &Path) -> PathBuf {
    repo_root.join(".oh").join(".cache").join("embeddings")
}

pub fn generations_root(repo_root: &Path) -> PathBuf {
    semantic_root(repo_root).join("generations")
}

pub fn generation_root(repo_root: &Path, digest: &str) -> Result<PathBuf> {
    require_sha256("generation digest", digest)?;
    Ok(generations_root(repo_root).join(digest))
}

pub fn current_pointer_path(repo_root: &Path) -> PathBuf {
    semantic_root(repo_root).join("current.json")
}

pub fn generation_digest(
    identity: &SemanticIdentity,
    canonical_input_digest: &str,
    target_graph_digest: &str,
    structural_graph_snapshot_digest: &str,
) -> Result<String> {
    identity.validate()?;
    require_sha256("canonical_input_digest", canonical_input_digest)?;
    require_sha256("target_graph_digest", target_graph_digest)?;
    require_sha256(
        "structural_graph_snapshot_digest",
        structural_graph_snapshot_digest,
    )?;
    let key = GenerationKey {
        schema_version: GENERATION_SCHEMA_VERSION,
        semantic_identity: identity,
        canonical_input_digest,
        target_graph_digest,
        structural_graph_snapshot_digest,
    };
    Ok(sha256_bytes(&canonical_json_bytes(&key)?))
}

pub fn canonical_input_digest(rows: impl IntoIterator<Item = (String, String)>) -> Result<String> {
    let mut rows = rows.into_iter().collect::<Vec<_>>();
    rows.sort();
    let mut seen = BTreeSet::new();
    for (id, digest) in &rows {
        if id.is_empty() || !seen.insert(id) {
            bail!("canonical semantic inputs contain an empty or duplicate id");
        }
        require_sha256("row canonical input digest", digest)?;
    }
    let serializable = rows
        .iter()
        .map(|(id, digest)| CanonicalInputRow {
            id,
            canonical_input_digest: digest,
        })
        .collect::<Vec<_>>();
    Ok(sha256_bytes(&canonical_json_bytes(&serializable)?))
}

pub fn vector_sha256(vector: &[f32], expected_dimension: usize) -> Result<String> {
    if vector.len() != expected_dimension {
        bail!(
            "semantic vector dimension mismatch: expected {}, observed {}",
            expected_dimension,
            vector.len()
        );
    }
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        if !value.is_finite() {
            bail!("semantic vector contains a non-finite value");
        }
        bytes.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    Ok(sha256_bytes(&bytes))
}

pub fn target_graph_digest(nodes: &[crate::graph::Node]) -> Result<String> {
    let mut nodes = nodes.to_vec();
    nodes.sort_by_key(crate::graph::Node::stable_id);
    let mut previous: Option<String> = None;
    for node in &nodes {
        let id = node.stable_id();
        if previous
            .as_deref()
            .is_some_and(|prior| prior >= id.as_str())
        {
            bail!("target graph contains duplicate or unsorted stable identities");
        }
        previous = Some(id);
    }
    Ok(sha256_bytes(&canonical_json_bytes(&nodes)?))
}

pub fn tree_digest(root: &Path) -> Result<String> {
    let entries = verified_tree_entries(root)?;
    Ok(sha256_bytes(&canonical_json_bytes(&entries)?))
}

fn verified_tree_entries(root: &Path) -> Result<Vec<TreeEntry>> {
    let root_metadata = fs::symlink_metadata(root)
        .with_context(|| format!("semantic tree is missing: {}", root.display()))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        bail!(
            "semantic tree root must be a regular directory: {}",
            root.display()
        );
    }
    let mut entries = Vec::new();
    collect_tree_entries(root, root, &mut entries)?;
    entries.sort();
    if entries.is_empty() {
        bail!("semantic tree is empty: {}", root.display());
    }
    Ok(entries)
}

fn collect_tree_entries(root: &Path, current: &Path, entries: &mut Vec<TreeEntry>) -> Result<()> {
    let mut children = fs::read_dir(current)
        .with_context(|| format!("failed to read semantic tree {}", current.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        let relative = path.strip_prefix(root).expect("child must be under root");
        // Validate every entry, including directories, before descending so a
        // non-UTF-8 empty directory cannot disappear from the digest surface.
        let _ = path_to_slash(relative)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            bail!(
                "semantic generation tree contains a symlink: {}",
                path.display()
            );
        }
        if metadata.is_dir() {
            collect_tree_entries(root, &path, entries)?;
        } else if metadata.is_file() {
            entries.push(TreeEntry {
                path: path_to_slash(relative)?,
                size: metadata.len(),
                sha256: sha256_file(&path)?,
            });
        } else {
            bail!(
                "semantic generation tree contains a special file: {}",
                path.display()
            );
        }
    }
    Ok(())
}

pub fn write_generation_evidence(
    generation_root: &Path,
    coverage: &CoverageManifest,
    manifest: &GenerationManifest,
) -> Result<String> {
    coverage.validate()?;
    manifest.validate()?;
    if coverage.generation_digest != manifest.generation_digest
        || coverage.semantic_identity_digest != manifest.semantic_identity_digest
        || coverage.canonical_input_digest != manifest.canonical_input_digest
        || coverage.target_graph_digest != manifest.target_graph_digest
        || coverage.structural_graph_snapshot_digest != manifest.structural_graph_snapshot_digest
        || coverage.row_count != manifest.row_count
    {
        bail!("semantic coverage and generation manifest identities do not match");
    }
    let coverage_bytes = canonical_json_bytes(coverage)?;
    if sha256_bytes(&coverage_bytes) != manifest.coverage_digest {
        bail!("semantic coverage digest does not match generation manifest");
    }
    let observed_tree_digest = tree_digest(&generation_root.join("lance"))?;
    if observed_tree_digest != manifest.lance_tree_digest {
        bail!("semantic Lance tree digest does not match generation manifest");
    }
    fs::create_dir_all(generation_root)?;
    write_atomic_bytes(&generation_root.join("coverage.json"), &coverage_bytes)?;
    let manifest_bytes = canonical_json_bytes(manifest)?;
    write_atomic_bytes(&generation_root.join("manifest.json"), &manifest_bytes)?;
    Ok(sha256_bytes(&manifest_bytes))
}

pub fn verify_generation_files(
    repo_root: &Path,
    generation_digest: &str,
    expected_manifest_sha256: Option<&str>,
) -> Result<(GenerationManifest, CoverageManifest, String)> {
    let root = generation_root(repo_root, generation_digest)?;
    let manifest_path = root.join("manifest.json");
    let coverage_path = root.join("coverage.json");
    let (manifest, manifest_bytes): (GenerationManifest, Vec<u8>) =
        read_canonical_json(&manifest_path)?;
    manifest.validate()?;
    if manifest.generation_digest != generation_digest {
        bail!("semantic generation directory identity does not match manifest");
    }
    let manifest_sha256 = sha256_bytes(&manifest_bytes);
    if expected_manifest_sha256.is_some_and(|expected| expected != manifest_sha256) {
        bail!("semantic generation manifest checksum mismatch");
    }
    let (coverage, coverage_bytes): (CoverageManifest, Vec<u8>) =
        read_canonical_json(&coverage_path)?;
    coverage.validate()?;
    if sha256_bytes(&coverage_bytes) != manifest.coverage_digest {
        bail!("semantic generation coverage checksum mismatch");
    }
    if coverage.generation_digest != manifest.generation_digest
        || coverage.semantic_identity_digest != manifest.semantic_identity_digest
        || coverage.canonical_input_digest != manifest.canonical_input_digest
        || coverage.target_graph_digest != manifest.target_graph_digest
        || coverage.structural_graph_snapshot_digest != manifest.structural_graph_snapshot_digest
        || coverage.row_count != manifest.row_count
    {
        bail!("semantic generation coverage identity does not match manifest");
    }
    if tree_digest(&root.join("lance"))? != manifest.lance_tree_digest {
        bail!("semantic generation Lance tree checksum mismatch");
    }
    Ok((manifest, coverage, manifest_sha256))
}

pub fn load_current_generation(
    repo_root: &Path,
) -> Result<
    Option<(
        CurrentPointer,
        GenerationManifest,
        CoverageManifest,
        SemanticVerificationReceipt,
    )>,
> {
    let path = current_pointer_path(repo_root);
    if !path.exists() {
        return Ok(None);
    }
    let (pointer, _): (CurrentPointer, Vec<u8>) = read_canonical_json(&path)?;
    pointer.validate()?;
    let (manifest, coverage, _) = verify_generation_files(
        repo_root,
        &pointer.generation_digest,
        Some(&pointer.manifest_sha256),
    )?;
    let (verification, verification_bytes): (SemanticVerificationReceipt, Vec<u8>) =
        read_canonical_json(
            &generation_root(repo_root, &pointer.generation_digest)?.join("verification.json"),
        )?;
    if sha256_bytes(&verification_bytes) != pointer.verification_sha256 {
        bail!("semantic verification receipt checksum mismatch");
    }
    verification.validate_against(&manifest)?;
    if verification.manifest_sha256 != pointer.manifest_sha256 {
        bail!("semantic verification receipt manifest checksum mismatch");
    }
    Ok(Some((pointer, manifest, coverage, verification)))
}

pub fn publish_current_generation(
    repo_root: &Path,
    generation_digest: &str,
    manifest_sha256: &str,
    verification: &SemanticVerificationReceipt,
) -> Result<()> {
    require_sha256("published generation digest", generation_digest)?;
    require_sha256("published manifest checksum", manifest_sha256)?;
    let (manifest, _, _) =
        verify_generation_files(repo_root, generation_digest, Some(manifest_sha256))?;
    verification.validate_against(&manifest)?;
    if verification.manifest_sha256 != manifest_sha256 {
        bail!("semantic verification manifest checksum mismatch");
    }
    let generation_root = generation_root(repo_root, generation_digest)?;
    let (persisted_verification, verification_bytes): (SemanticVerificationReceipt, Vec<u8>) =
        read_canonical_json(&generation_root.join("verification.json"))?;
    if persisted_verification != *verification {
        bail!("persisted semantic verification receipt does not match publication request");
    }
    let pointer = CurrentPointer {
        schema_version: CURRENT_POINTER_SCHEMA_VERSION,
        generation_digest: generation_digest.to_string(),
        manifest_sha256: manifest_sha256.to_string(),
        verification_sha256: sha256_bytes(&verification_bytes),
    };
    let bytes = canonical_json_bytes(&pointer)?;
    write_current_pointer_atomic(&current_pointer_path(repo_root), &bytes)
}

pub fn write_verification_evidence(
    generation_root: &Path,
    verification: &SemanticVerificationReceipt,
    manifest: &GenerationManifest,
) -> Result<String> {
    verification.validate_against(manifest)?;
    if sha256_file(&generation_root.join("manifest.json"))? != verification.manifest_sha256 {
        bail!("semantic verification receipt does not bind the staged manifest bytes");
    }
    let bytes = canonical_json_bytes(verification)?;
    write_atomic_bytes(&generation_root.join("verification.json"), &bytes)?;
    Ok(sha256_bytes(&bytes))
}

pub fn promote_staging_generation(
    repo_root: &Path,
    staging_root: &Path,
    generation_digest: &str,
) -> Result<PathBuf> {
    let semantic_root = semantic_root(repo_root);
    let canonical_staging = fs::canonicalize(staging_root).with_context(|| {
        format!(
            "failed to canonicalize semantic staging root {}",
            staging_root.display()
        )
    })?;
    let canonical_semantic = fs::canonicalize(&semantic_root).with_context(|| {
        format!(
            "failed to canonicalize semantic root {}",
            semantic_root.display()
        )
    })?;
    if !canonical_staging.starts_with(&canonical_semantic) {
        bail!("semantic staging root escapes the repository semantic cache");
    }
    let destination = generation_root(repo_root, generation_digest)?;
    fs::create_dir_all(generations_root(repo_root))?;
    if destination.exists() {
        let (_, _, existing_sha) = verify_generation_files(repo_root, generation_digest, None)?;
        let staging_manifest_sha = sha256_file(&staging_root.join("manifest.json"))?;
        if existing_sha != staging_manifest_sha {
            bail!("immutable semantic generation already exists with different content");
        }
        return Ok(destination);
    }
    fs::rename(staging_root, &destination).with_context(|| {
        format!(
            "failed to atomically promote semantic generation {}",
            generation_digest
        )
    })?;
    sync_directory(destination.parent().expect("generation has parent"))?;
    Ok(destination)
}

pub fn new_staging_root(repo_root: &Path, generation_digest: &str) -> Result<PathBuf> {
    require_sha256("staged generation digest", generation_digest)?;
    let staging_parent = semantic_root(repo_root).join("staging");
    fs::create_dir_all(&staging_parent)?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let staging = staging_parent.join(format!(
        "{}.{}.{}",
        generation_digest,
        std::process::id(),
        sequence
    ));
    fs::create_dir(&staging).with_context(|| {
        format!(
            "failed to create isolated semantic generation staging directory {}",
            staging.display()
        )
    })?;
    Ok(staging)
}

pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value)?;
    let mut output = String::new();
    write_canonical_json_value(&value, &mut output)?;
    output.push('\n');
    Ok(output.into_bytes())
}

fn write_canonical_json_value(value: &serde_json::Value, output: &mut String) -> Result<()> {
    match value {
        serde_json::Value::Null => output.push_str("null"),
        serde_json::Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        serde_json::Value::Number(value) => output.push_str(&value.to_string()),
        serde_json::Value::String(value) => output.push_str(&serde_json::to_string(value)?),
        serde_json::Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical_json_value(value, output)?;
            }
            output.push(']');
        }
        serde_json::Value::Object(values) => {
            output.push('{');
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key)?);
                output.push(':');
                write_canonical_json_value(value, output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn read_canonical_json<T: for<'de> Deserialize<'de> + Serialize>(
    path: &Path,
) -> Result<(T, Vec<u8>)> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("missing semantic generation file {}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!(
            "semantic generation evidence must be a regular file: {}",
            path.display()
        );
    }
    let bytes = fs::read(path)?;
    let value: T = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid semantic generation JSON {}", path.display()))?;
    if canonical_json_bytes(&value)? != bytes {
        bail!(
            "semantic generation JSON is not canonical: {}",
            path.display()
        );
    }
    Ok((value, bytes))
}

fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("semantic evidence path has no parent"))?;
    fs::create_dir_all(parent)?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("semantic-evidence"),
        std::process::id(),
        sequence
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn write_current_pointer_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    write_current_pointer_atomic_with_sync(path, bytes, |directory| directory.sync_all())
}

fn write_current_pointer_atomic_with_sync<F>(path: &Path, bytes: &[u8], sync: F) -> Result<()>
where
    F: FnOnce(&File) -> std::io::Result<()>,
{
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("semantic current pointer path has no parent"))?;
    fs::create_dir_all(parent)?;

    // Open the directory before the rename so every operation that can prevent
    // publication fails while the prior pointer is still active.
    let directory = File::open(parent)?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("semantic-current"),
        std::process::id(),
        sequence
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;

        // The rename is the publication commit point. Directory fsync improves
        // crash durability, but after the rename succeeds there is no reliable
        // rollback to the prior pointer. Consequently a post-commit fsync error
        // must not turn a successful pointer change into a reported failure.
        fs::rename(&temp, path)?;
        let _ = sync(&directory);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn current_executable_sha256() -> Result<String> {
    let executable = std::env::current_exe().context("failed to resolve running RNA artifact")?;
    sha256_file(&executable).with_context(|| {
        format!(
            "failed to hash running RNA artifact {}",
            executable.display()
        )
    })
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("checksum target must be a regular file: {}", path.display());
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn validate_digest(name: &str, value: &str) -> Result<()> {
    require_sha256(name, value)
}

fn require_sha256(name: &str, value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!("{} must be a lowercase hexadecimal SHA-256 digest", name);
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!("semantic tree member path must be non-empty and relative");
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("semantic tree member path contains traversal");
        }
    }
    Ok(())
}

fn path_to_slash(path: &Path) -> Result<String> {
    validate_relative_path(path)?;
    path.components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("semantic tree member path is not UTF-8"))
        })
        .collect::<Result<Vec<_>>>()
        .map(|components| components.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> String {
        format!("{byte:02x}").repeat(32)
    }

    fn identity() -> SemanticIdentity {
        SemanticIdentity {
            model: "model".to_string(),
            tokenizer: "tokenizer".to_string(),
            model_files_digest: digest(70),
            model_sha256: digest(71),
            tokenizer_sha256: digest(72),
            reranker_files_digest: digest(73),
            preprocessing_version: "preprocessing".to_string(),
            artifact_sha256: digest(1),
            schema_signature: SEMANTIC_SCHEMA_SIGNATURE.to_string(),
            dimension: 3,
            flags: BTreeMap::from([("scan".to_string(), "full".to_string())]),
        }
    }

    fn attestation() -> DeviceAttestation {
        DeviceAttestation {
            required_device: "metal".to_string(),
            observed_device: "metal".to_string(),
            backend: "candle-metal".to_string(),
            device_index: Some(0),
            artifact_sha256: digest(1),
        }
    }

    fn publish_test_generation(
        repo: &Path,
        identity: &SemanticIdentity,
        row: &CoverageRow,
        target_graph_digest: String,
        structural_graph_snapshot_digest: String,
        reused_vector_count: usize,
        encoded_vector_count: usize,
        prior_generation_digest: Option<String>,
    ) -> GenerationManifest {
        let semantic_identity_digest = identity.digest().unwrap();
        let canonical_input_digest =
            canonical_input_digest([(row.id.clone(), row.canonical_input_digest.clone())]).unwrap();
        let generation_digest = generation_digest(
            identity,
            &canonical_input_digest,
            &target_graph_digest,
            &structural_graph_snapshot_digest,
        )
        .unwrap();
        let staging = new_staging_root(repo, &generation_digest).unwrap();
        fs::create_dir(staging.join("lance")).unwrap();
        fs::write(
            staging.join("lance/data"),
            format!("immutable-vector:{}", row.vector_sha256),
        )
        .unwrap();
        let coverage = CoverageManifest {
            schema_version: COVERAGE_SCHEMA_VERSION,
            generation_digest: generation_digest.clone(),
            semantic_identity_digest: semantic_identity_digest.clone(),
            canonical_input_digest: canonical_input_digest.clone(),
            target_graph_digest: target_graph_digest.clone(),
            structural_graph_snapshot_digest: structural_graph_snapshot_digest.clone(),
            row_count: 1,
            rows: vec![row.clone()],
        };
        let coverage_digest = coverage.digest().unwrap();
        let lance_tree_digest = tree_digest(&staging.join("lance")).unwrap();
        let manifest = GenerationManifest {
            schema_version: GENERATION_SCHEMA_VERSION,
            generation_digest: generation_digest.clone(),
            semantic_identity: identity.clone(),
            semantic_identity_digest,
            canonical_input_digest,
            target_graph_digest: target_graph_digest.clone(),
            structural_graph_snapshot_digest: structural_graph_snapshot_digest.clone(),
            row_count: 1,
            coverage_digest: coverage_digest.clone(),
            lance_tree_digest: lance_tree_digest.clone(),
            reused_vector_count,
            encoded_vector_count,
            prior_generation_digest,
            created_by_artifact_sha256: identity.artifact_sha256.clone(),
            device_attestation: attestation(),
        };
        let manifest_sha = write_generation_evidence(&staging, &coverage, &manifest).unwrap();
        let verification = SemanticVerificationReceipt {
            schema_version: VERIFICATION_SCHEMA_VERSION,
            generation_digest: generation_digest.clone(),
            manifest_sha256: manifest_sha.clone(),
            coverage_digest,
            lance_tree_digest,
            structural_graph_snapshot_digest,
            target_graph_digest,
            row_count: 1,
            one_to_one_coverage: true,
            fresh_reopen_ready: true,
        };
        write_verification_evidence(&staging, &verification, &manifest).unwrap();
        promote_staging_generation(repo, &staging, &generation_digest).unwrap();
        publish_current_generation(repo, &generation_digest, &manifest_sha, &verification).unwrap();
        manifest
    }

    #[test]
    fn canonical_json_is_stable_and_newline_terminated() {
        let value = BTreeMap::from([
            ("z".to_string(), serde_json::json!({"b": 2, "a": 1})),
            ("a".to_string(), serde_json::json!([3, 2, 1])),
        ]);
        assert_eq!(
            canonical_json_bytes(&value).unwrap(),
            br#"{"a":[3,2,1],"z":{"a":1,"b":2}}
"#
        );
    }

    #[test]
    fn vector_hash_rejects_partial_short_wrong_dimension_and_nonfinite_output() {
        assert!(vector_sha256(&[], 3).is_err());
        assert!(vector_sha256(&[1.0, 2.0], 3).is_err());
        assert!(vector_sha256(&[1.0, 2.0, 3.0, 4.0], 3).is_err());
        assert!(vector_sha256(&[1.0, f32::NAN, 3.0], 3).is_err());
        assert!(vector_sha256(&[1.0, f32::INFINITY, 3.0], 3).is_err());
        assert_eq!(vector_sha256(&[1.0, 2.0, 3.0], 3).unwrap().len(), 64);
    }

    #[test]
    fn identity_mismatch_requires_rebuild_without_migration() {
        let input = digest(2);
        let graph = digest(3);
        let structural = digest(4);
        let original = generation_digest(&identity(), &input, &graph, &structural).unwrap();
        let mut changed = identity();
        changed.tokenizer.push_str("-changed");
        assert_ne!(
            original,
            generation_digest(&changed, &input, &graph, &structural).unwrap()
        );
        changed.schema_signature = "old-schema".to_string();
        assert!(generation_digest(&changed, &input, &graph, &structural).is_err());
    }

    #[test]
    fn coverage_rejects_duplicate_missing_and_orphan_accounting() {
        let row = CoverageRow {
            id: "node".to_string(),
            canonical_input_digest: digest(4),
            vector_sha256: digest(5),
        };
        let canonical_input_digest =
            canonical_input_digest([(row.id.clone(), row.canonical_input_digest.clone())]).unwrap();
        let mut coverage = CoverageManifest {
            schema_version: COVERAGE_SCHEMA_VERSION,
            generation_digest: digest(6),
            semantic_identity_digest: identity().digest().unwrap(),
            canonical_input_digest,
            target_graph_digest: digest(7),
            structural_graph_snapshot_digest: digest(8),
            row_count: 1,
            rows: vec![row.clone()],
        };
        coverage.validate().unwrap();
        coverage.rows.push(row);
        coverage.row_count = 2;
        assert!(coverage.validate().is_err());
        coverage.rows.pop();
        coverage.row_count = 2;
        assert!(coverage.validate().is_err());
    }

    #[test]
    fn strict_device_attestation_rejects_cpu_fallback() {
        let mut cpu = attestation();
        cpu.observed_device = "cpu".to_string();
        cpu.backend = "candle-cpu".to_string();
        assert!(cpu.validate(&identity()).is_err());
    }

    #[test]
    fn identical_target_reuses_every_exact_vector() {
        let identity = identity();
        let prior = vec![
            CoverageRow {
                id: "a".to_string(),
                canonical_input_digest: digest(20),
                vector_sha256: digest(21),
            },
            CoverageRow {
                id: "b".to_string(),
                canonical_input_digest: digest(22),
                vector_sha256: digest(23),
            },
        ];
        let target = vec![("a".to_string(), digest(20)), ("b".to_string(), digest(22))];
        let plan = plan_vector_reuse(Some(&identity), &identity, &prior, &target).unwrap();
        assert_eq!(plan.reused_ids, ["a", "b"]);
        assert!(plan.encode_ids.is_empty());
    }

    #[test]
    fn value_addressed_add_change_delete_and_rename_reuse() {
        let identity = identity();
        let prior = vec![
            CoverageRow {
                id: "unchanged".to_string(),
                canonical_input_digest: digest(30),
                vector_sha256: digest(31),
            },
            CoverageRow {
                id: "changed".to_string(),
                canonical_input_digest: digest(32),
                vector_sha256: digest(33),
            },
            CoverageRow {
                id: "deleted".to_string(),
                canonical_input_digest: digest(34),
                vector_sha256: digest(35),
            },
            CoverageRow {
                id: "old-name".to_string(),
                canonical_input_digest: digest(36),
                vector_sha256: digest(37),
            },
        ];
        let target = vec![
            ("unchanged".to_string(), digest(30)),
            ("changed".to_string(), digest(40)),
            ("added".to_string(), digest(41)),
            ("new-name".to_string(), digest(36)),
        ];
        let plan = plan_vector_reuse(Some(&identity), &identity, &prior, &target).unwrap();
        assert_eq!(plan.reused_ids, ["new-name", "unchanged"]);
        assert_eq!(plan.encode_ids, ["added", "changed"]);
        assert!(!plan.reused_ids.iter().any(|id| id == "deleted"));
        assert!(!plan.reused_ids.iter().any(|id| id == "old-name"));
    }

    #[test]
    fn semantic_identity_mismatch_rebuilds_every_target_row() {
        let original = identity();
        let mut changed = original.clone();
        changed
            .flags
            .insert("scan".to_string(), "incremental".to_string());
        let prior = vec![CoverageRow {
            id: "a".to_string(),
            canonical_input_digest: digest(50),
            vector_sha256: digest(51),
        }];
        let target = vec![("a".to_string(), digest(50))];
        let plan = plan_vector_reuse(Some(&original), &changed, &prior, &target).unwrap();
        assert!(plan.reused_ids.is_empty());
        assert_eq!(plan.encode_ids, ["a"]);
    }

    #[test]
    fn stale_graph_generation_vectors_publish_into_a_new_graph_binding() {
        let temp = tempfile::tempdir().unwrap();
        let identity = identity();
        let row = CoverageRow {
            id: "unchanged".to_string(),
            canonical_input_digest: digest(80),
            vector_sha256: digest(81),
        };
        let prior = publish_test_generation(
            temp.path(),
            &identity,
            &row,
            digest(82),
            digest(83),
            0,
            1,
            None,
        );

        let reuse = plan_vector_reuse(
            Some(&identity),
            &identity,
            std::slice::from_ref(&row),
            &[(row.id.clone(), row.canonical_input_digest.clone())],
        )
        .unwrap();
        assert_eq!(reuse.reused_ids, ["unchanged"]);
        assert!(reuse.encode_ids.is_empty());

        let target = publish_test_generation(
            temp.path(),
            &identity,
            &row,
            digest(84),
            digest(85),
            reuse.reused_ids.len(),
            reuse.encode_ids.len(),
            Some(prior.generation_digest.clone()),
        );
        let (_, published, coverage, _) = load_current_generation(temp.path())
            .unwrap()
            .expect("target generation must be published");

        assert_ne!(published.target_graph_digest, prior.target_graph_digest);
        assert_eq!(published, target);
        assert_eq!(
            published.prior_generation_digest,
            Some(prior.generation_digest)
        );
        assert_eq!(published.reused_vector_count, 1);
        assert_eq!(published.encoded_vector_count, 0);
        assert_eq!(coverage.rows, [row]);
    }

    #[test]
    fn tree_digest_rejects_symlinks_and_detects_tampering() {
        let temp = tempfile::tempdir().unwrap();
        let tree = temp.path().join("lance");
        fs::create_dir(&tree).unwrap();
        fs::write(tree.join("data"), b"one").unwrap();
        let before = tree_digest(&tree).unwrap();
        fs::write(tree.join("data"), b"two").unwrap();
        assert_ne!(before, tree_digest(&tree).unwrap());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(tree.join("data"), tree.join("link")).unwrap();
            assert!(tree_digest(&tree).is_err());
        }
    }

    #[test]
    fn failed_pointer_publication_leaves_prior_pointer_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let pointer_path = current_pointer_path(root);
        fs::create_dir_all(pointer_path.parent().unwrap()).unwrap();
        fs::write(&pointer_path, b"prior-pointer\n").unwrap();

        let verification = SemanticVerificationReceipt {
            schema_version: VERIFICATION_SCHEMA_VERSION,
            generation_digest: digest(8),
            manifest_sha256: digest(9),
            coverage_digest: digest(10),
            lance_tree_digest: digest(11),
            structural_graph_snapshot_digest: digest(12),
            target_graph_digest: digest(13),
            row_count: 0,
            one_to_one_coverage: true,
            fresh_reopen_ready: true,
        };
        assert!(publish_current_generation(root, &digest(8), &digest(9), &verification).is_err());
        assert_eq!(fs::read(pointer_path).unwrap(), b"prior-pointer\n");
    }

    #[test]
    fn post_rename_directory_sync_failure_is_committed_success() {
        let temp = tempfile::tempdir().unwrap();
        let pointer_path = current_pointer_path(temp.path());
        fs::create_dir_all(pointer_path.parent().unwrap()).unwrap();
        fs::write(&pointer_path, b"prior-pointer\n").unwrap();

        let result =
            write_current_pointer_atomic_with_sync(&pointer_path, b"new-pointer\n", |_| {
                Err(std::io::Error::other("injected post-rename sync failure"))
            });

        assert!(result.is_ok());
        assert_eq!(fs::read(pointer_path).unwrap(), b"new-pointer\n");
    }

    #[test]
    fn published_generation_reopen_verifies_every_bound_file_and_rejects_tampering() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let identity = identity();
        let identity_digest = identity.digest().unwrap();
        let row = CoverageRow {
            id: "node".to_string(),
            canonical_input_digest: digest(60),
            vector_sha256: digest(61),
        };
        let input_digest =
            canonical_input_digest([(row.id.clone(), row.canonical_input_digest.clone())]).unwrap();
        let target_graph_digest = digest(62);
        let structural_graph_snapshot_digest = digest(63);
        let generation_digest = generation_digest(
            &identity,
            &input_digest,
            &target_graph_digest,
            &structural_graph_snapshot_digest,
        )
        .unwrap();
        let staging = new_staging_root(repo, &generation_digest).unwrap();
        fs::create_dir(staging.join("lance")).unwrap();
        fs::write(staging.join("lance/data"), b"immutable-lance").unwrap();
        let coverage = CoverageManifest {
            schema_version: COVERAGE_SCHEMA_VERSION,
            generation_digest: generation_digest.clone(),
            semantic_identity_digest: identity_digest.clone(),
            canonical_input_digest: input_digest.clone(),
            target_graph_digest: target_graph_digest.clone(),
            structural_graph_snapshot_digest: structural_graph_snapshot_digest.clone(),
            row_count: 1,
            rows: vec![row],
        };
        let coverage_digest = coverage.digest().unwrap();
        let lance_tree_digest = tree_digest(&staging.join("lance")).unwrap();
        let manifest = GenerationManifest {
            schema_version: GENERATION_SCHEMA_VERSION,
            generation_digest: generation_digest.clone(),
            semantic_identity: identity.clone(),
            semantic_identity_digest: identity_digest,
            canonical_input_digest: input_digest,
            target_graph_digest: target_graph_digest.clone(),
            structural_graph_snapshot_digest: structural_graph_snapshot_digest.clone(),
            row_count: 1,
            coverage_digest: coverage_digest.clone(),
            lance_tree_digest: lance_tree_digest.clone(),
            reused_vector_count: 0,
            encoded_vector_count: 1,
            prior_generation_digest: None,
            created_by_artifact_sha256: identity.artifact_sha256.clone(),
            device_attestation: attestation(),
        };
        let manifest_sha = write_generation_evidence(&staging, &coverage, &manifest).unwrap();
        let verification = SemanticVerificationReceipt {
            schema_version: VERIFICATION_SCHEMA_VERSION,
            generation_digest: generation_digest.clone(),
            manifest_sha256: manifest_sha.clone(),
            coverage_digest,
            lance_tree_digest,
            structural_graph_snapshot_digest,
            target_graph_digest,
            row_count: 1,
            one_to_one_coverage: true,
            fresh_reopen_ready: true,
        };
        write_verification_evidence(&staging, &verification, &manifest).unwrap();
        let promoted = promote_staging_generation(repo, &staging, &generation_digest).unwrap();
        publish_current_generation(repo, &generation_digest, &manifest_sha, &verification).unwrap();
        assert!(load_current_generation(repo).unwrap().is_some());

        fs::write(promoted.join("coverage.json"), b"{}\n").unwrap();
        assert!(load_current_generation(repo).is_err());
    }
}
