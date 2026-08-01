//! Verifier-owned structural-cache identity and incremental reuse planning.
//!
//! The portable archive is created and safely transported by the frozen-cohort
//! qualifier. This module is the producer-side trust boundary: it publishes
//! the exact binary/schema identity, verifies an injected authorization against
//! the current Git tree and descriptor partitions, and computes the graph
//! impact closure that must be executed before inherited evidence is admitted.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::business_context::BusinessContextMode;
use crate::extract::lsp::MAX_INCREMENTAL_LSP_OPERATIONS;
use crate::graph::{Edge, EdgeKind, Node};
use crate::roots::RootConfig;

pub const STRUCTURAL_CACHE_IDENTITY_SCHEMA_VERSION: u32 = 1;
pub const STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION: u32 = 2;
pub const STRUCTURAL_CACHE_AUTHORIZATION_PATH: &str =
    ".oh/.cache/structural-cache-inheritance.json";
pub const STRUCTURAL_CACHE_EXECUTION_PATH: &str = ".oh/.cache/structural-cache-execution.json";
pub const STRUCTURAL_CACHE_AUTHORIZATION_SHA256_ENV: &str =
    "RNA_STRUCTURAL_CACHE_AUTHORIZATION_SHA256";
pub const QUALIFICATION_SCAN_FLAGS: &[&str] = &[
    "--business-context=disabled",
    "scan",
    "--full",
    "--no-embed",
    "--timings",
];
const MAX_IMPACT_FILES: usize = 4_096;
const MAX_IMPACT_FRACTION_NUMERATOR: usize = 1;
const MAX_IMPACT_FRACTION_DENOMINATOR: usize = 2;
pub const VERIFIED_OPERATION_BUDGET_BASIS: &str =
    "verified_base_capability_aware_per_path_work_ledger_with_language_median_for_unseen_paths";
const SHARED_INFLUENCE_PATTERNS: &[&str] = &[
    ".oh/config.toml",
    ".editorconfig",
    ".lsp.json",
    "BUILD",
    "BUILD.bazel",
    "MODULE.bazel",
    "WORKSPACE",
    "WORKSPACE.bazel",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StructuralProducerIdentity {
    pub producer_commit: String,
    pub package_version: String,
    pub binary_sha256: String,
    pub graph_schema_version: u32,
    pub graph_schema_signature: String,
    pub completeness_schema_version: u32,
    pub work_item_schema_version: u32,
    pub validation_evidence_schema_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguagePartitionIdentity {
    pub language: String,
    pub descriptor_signature: String,
    pub influence_patterns: Vec<String>,
    pub influence_digest: String,
    pub signature: String,
    pub matched_file_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StructuralCacheIdentity {
    pub schema_version: u32,
    pub repository: String,
    pub commit: String,
    pub tree: String,
    pub root_slug: String,
    pub configuration_digest: String,
    pub inventory_policy_digest: String,
    pub context_mode: String,
    pub producer: StructuralProducerIdentity,
    pub shared_influence_digest: String,
    pub partitions: BTreeMap<String, LanguagePartitionIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct InheritedResultProducer {
    pub result_id: String,
    pub producer_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InheritedFileAuthorization {
    pub path: String,
    pub blob: String,
    pub language: String,
    pub partition_signature: String,
    pub base_file_sha256: String,
    pub input_hashes: Vec<String>,
    pub operations: Vec<String>,
    pub producer_work_ids: Vec<String>,
    pub producer_graph_enrichment_operation_count: u64,
    pub expected_result_ids: Vec<String>,
    pub result_producers: Vec<InheritedResultProducer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StructuralCacheOperationBudget {
    pub max_operations: u64,
    pub executed_estimate: u64,
    pub authorized_operations_by_language: BTreeMap<String, Vec<String>>,
    pub basis: String,
    pub estimated_file_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StructuralCacheAuthorization {
    pub schema_version: u32,
    pub offline_preprocessing: bool,
    pub repository: String,
    pub base_commit: String,
    pub base_tree: String,
    pub target_commit: String,
    pub target_tree: String,
    pub root_slug: String,
    pub producer: StructuralProducerIdentity,
    pub toolchain_lock_digest: String,
    pub inventory_digest: String,
    pub inventory_file_sha256: String,
    pub configuration_digest: String,
    pub scan_flags: Vec<String>,
    pub base_archive_sha256: String,
    pub base_sidecar_sha256: String,
    pub base_core_sha256: String,
    pub base_report_digest: String,
    pub base_report_sha256: String,
    pub inherited_files: Vec<InheritedFileAuthorization>,
    pub changed_paths: Vec<String>,
    pub added_paths: Vec<String>,
    pub deleted_paths: Vec<String>,
    pub renamed_paths: Vec<[String; 2]>,
    pub invalidated_partitions: Vec<String>,
    pub invalidated_paths: Vec<String>,
    pub path_partitions: BTreeMap<String, String>,
    pub executed_operation_budget: StructuralCacheOperationBudget,
    pub digest: String,
}

impl StructuralCacheAuthorization {
    pub fn finalize(&mut self) -> Result<()> {
        self.inherited_files
            .sort_by(|left, right| left.path.cmp(&right.path));
        self.changed_paths.sort();
        self.changed_paths.dedup();
        self.added_paths.sort();
        self.added_paths.dedup();
        self.deleted_paths.sort();
        self.deleted_paths.dedup();
        self.renamed_paths.sort();
        self.renamed_paths.dedup();
        self.invalidated_partitions.sort();
        self.invalidated_partitions.dedup();
        self.invalidated_paths.sort();
        self.invalidated_paths.dedup();
        self.digest.clear();
        self.digest = canonical_json_sha256(self)?;
        Ok(())
    }

    pub fn verify_digest(&self) -> bool {
        let expected = self.digest.clone();
        let mut candidate = self.clone();
        candidate.finalize().is_ok() && candidate.digest == expected
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StructuralCacheExecution {
    pub schema_version: u32,
    pub offline_preprocessing: bool,
    pub base_archive_sha256: String,
    pub base_sidecar_sha256: String,
    pub base_report_digest: String,
    pub target_commit: String,
    pub target_tree: String,
    pub inherited_paths: Vec<String>,
    pub executed_paths: Vec<String>,
    pub invalidated_partitions: Vec<String>,
    pub escalated_partitions: Vec<String>,
    pub changed_file_count: u64,
    pub invalidated_file_count: u64,
    pub inherited_graph_enrichment_operation_count: u64,
    pub executed_graph_enrichment_operation_count: u64,
    pub inherited_readiness_validation_request_count: u64,
    pub executed_readiness_validation_request_count: u64,
    pub executed_producer_work_ids: Vec<String>,
    pub closure_edge_count: u64,
    pub execution_job_id: Option<String>,
    pub digest: String,
}

impl StructuralCacheExecution {
    pub fn finalize(&mut self) {
        self.inherited_paths.sort();
        self.inherited_paths.dedup();
        self.executed_paths.sort();
        self.executed_paths.dedup();
        self.invalidated_partitions.sort();
        self.invalidated_partitions.dedup();
        self.escalated_partitions.sort();
        self.escalated_partitions.dedup();
        self.executed_producer_work_ids.sort();
        self.executed_producer_work_ids.dedup();
        self.digest.clear();
        let bytes = serde_json::to_vec(self).expect("cache execution serialization cannot fail");
        self.digest = hex_sha256(&bytes);
    }

    pub fn verify_digest(&self) -> bool {
        let mut candidate = self.clone();
        let expected = candidate.digest.clone();
        candidate.finalize();
        candidate.digest == expected
    }
}

#[derive(Debug, Clone)]
pub struct VerifiedStructuralCacheAuthorization {
    pub authorization: StructuralCacheAuthorization,
    pub inherited_by_path: BTreeMap<String, InheritedFileAuthorization>,
    pub base_report: crate::lsp_completeness::LspCompletenessReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeStructuralCacheOperationAdmission {
    language: String,
    allowed_operations: BTreeSet<String>,
    max_operations: usize,
    signed_estimate: usize,
}

impl RuntimeStructuralCacheOperationAdmission {
    pub(crate) fn allows(&self, operation: &str) -> bool {
        self.allowed_operations.contains(operation)
    }

    pub(crate) fn validate_exact_plan<'a>(
        &self,
        operations: impl IntoIterator<Item = &'a str>,
    ) -> Result<usize> {
        let operations = operations.into_iter().collect::<Vec<_>>();
        if let Some(operation) = operations.iter().find(|operation| !self.allows(operation)) {
            bail!(
                "runtime LSP operation {operation} is outside the signed structural-cache admission for {}",
                self.language
            );
        }
        ensure!(
            operations.len() <= self.max_operations,
            "runtime LSP plan for {} exceeds the signed structural-cache ceiling (actual={} max={})",
            self.language,
            operations.len(),
            self.max_operations,
        );
        Ok(operations.len())
    }

    pub(crate) fn signed_estimate(&self) -> usize {
        self.signed_estimate
    }

    #[cfg(test)]
    pub(crate) fn for_test(allowed_operations: &[&str], max_operations: usize) -> Self {
        Self {
            language: "test-language".to_string(),
            allowed_operations: allowed_operations
                .iter()
                .map(|operation| (*operation).to_string())
                .collect(),
            max_operations,
            signed_estimate: 0,
        }
    }
}

impl VerifiedStructuralCacheAuthorization {
    pub fn signed_operation_budget(&self) -> Result<&StructuralCacheOperationBudget> {
        validate_executed_operation_budget(&self.authorization, &self.inherited_by_path)?;
        Ok(&self.authorization.executed_operation_budget)
    }

    pub fn signed_executed_operation_count(&self) -> Result<usize> {
        self.signed_operation_budget().and_then(|budget| {
            usize::try_from(budget.executed_estimate)
                .context("signed structural-cache operation estimate does not fit usize")
        })
    }

    pub fn inherited_readiness_validation_requests_by_language(
        &self,
        inherited_paths: &BTreeSet<PathBuf>,
    ) -> Result<BTreeMap<String, u64>> {
        let mut base_paths_by_language = BTreeMap::<String, BTreeSet<String>>::new();
        for file in self
            .base_report
            .files
            .iter()
            .filter(|file| file.role.is_included())
        {
            let language = file.language.as_deref().with_context(|| {
                format!("included base report file has no language: {}", file.path)
            })?;
            base_paths_by_language
                .entry(language.to_string())
                .or_default()
                .insert(file.path.clone());
        }

        let mut inherited_counts = BTreeMap::<String, u64>::new();
        for path in inherited_paths {
            let path = path.to_string_lossy();
            let file = self
                .inherited_by_path
                .get(path.as_ref())
                .with_context(|| format!("missing inherited authorization for {path}"))?;
            let base_paths = base_paths_by_language
                .get(file.language.as_str())
                .with_context(|| format!("base report has no {} partition", file.language))?;
            ensure!(
                base_paths.contains(file.path.as_str()),
                "base report has no inherited readiness path {}",
                file.path
            );
            *inherited_counts.entry(file.language.clone()).or_default() += 1;
        }

        let mut requests_by_language = BTreeMap::new();
        for (language, inherited_count) in inherited_counts {
            let base_file_count = base_paths_by_language[&language].len() as u64;
            let base_request_count = *self
                .base_report
                .readiness_validation_requests_by_language
                .get(&language)
                .with_context(|| {
                    format!("base report has no readiness request count for {language}")
                })?;
            ensure!(
                base_request_count >= base_file_count,
                "base readiness request count for {language} is smaller than its file count"
            );
            let partition_request_count = base_request_count - base_file_count;
            ensure!(
                partition_request_count <= 1,
                "base readiness request count for {language} has ambiguous non-file evidence"
            );

            // A verifier-clean qualification emits one file-scoped validation
            // per included file. Its one optional non-file initialization probe
            // is reusable only when the entire language partition is inherited;
            // a mixed partition executes a fresh probe for its changed paths.
            let mut request_count = inherited_count;
            if inherited_count == base_file_count {
                request_count += partition_request_count;
            }
            requests_by_language.insert(language, request_count);
        }
        Ok(requests_by_language)
    }

    pub fn inherited_readiness_validation_request_count(
        &self,
        inherited_paths: &BTreeSet<PathBuf>,
    ) -> Result<u64> {
        Ok(self
            .inherited_readiness_validation_requests_by_language(inherited_paths)?
            .values()
            .sum())
    }
}

fn validate_runtime_authorization_presence(
    authorization_present: bool,
    signed_marker_present: bool,
) -> Result<bool> {
    ensure!(
        authorization_present || !signed_marker_present,
        "signed structural-cache runtime marker is present but its authorization file is missing"
    );
    Ok(authorization_present)
}

static VERIFIED_RUNTIME_AUTHORIZATIONS: OnceLock<Mutex<BTreeMap<PathBuf, String>>> =
    OnceLock::new();

fn runtime_authorization_key(repo_root: &Path) -> Result<PathBuf> {
    repo_root.canonicalize().with_context(|| {
        format!(
            "resolve structural-cache runtime root {}",
            repo_root.display()
        )
    })
}

fn attest_runtime_authorization(repo_root: &Path, authorization_sha256: &str) -> Result<()> {
    let key = runtime_authorization_key(repo_root)?;
    VERIFIED_RUNTIME_AUTHORIZATIONS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| anyhow::anyhow!("structural-cache runtime attestation lock is poisoned"))?
        .insert(key, authorization_sha256.to_string());
    Ok(())
}

fn require_runtime_authorization_attestation(
    repo_root: &Path,
    authorization_sha256: &str,
) -> Result<()> {
    let key = runtime_authorization_key(repo_root)?;
    let verified = VERIFIED_RUNTIME_AUTHORIZATIONS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| anyhow::anyhow!("structural-cache runtime attestation lock is poisoned"))?;
    ensure!(
        verified.get(&key).map(String::as_str) == Some(authorization_sha256),
        "structural-cache runtime admission lacks an exact pre-request verifier attestation"
    );
    Ok(())
}

pub(crate) fn load_runtime_operation_admission(
    repo_root: &Path,
    language: &str,
) -> Result<Option<RuntimeStructuralCacheOperationAdmission>> {
    if !validate_runtime_authorization_presence(
        authorization_path(repo_root).is_file(),
        std::env::var_os(STRUCTURAL_CACHE_AUTHORIZATION_SHA256_ENV).is_some(),
    )? {
        return Ok(None);
    }
    let path = authorization_path(repo_root);
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "read runtime structural cache authorization {}",
            path.display()
        )
    })?;
    let expected_authorization_sha256 = std::env::var(STRUCTURAL_CACHE_AUTHORIZATION_SHA256_ENV)
        .context("signed structural-cache runtime marker disappeared")?;
    require_sha256(
        &expected_authorization_sha256,
        "runtime structural cache authorization handoff",
    )?;
    let actual_authorization_sha256 = hex_sha256(&bytes);
    ensure!(
        actual_authorization_sha256 == expected_authorization_sha256,
        "structural-cache authorization changed after verifier preflight"
    );
    require_runtime_authorization_attestation(repo_root, &actual_authorization_sha256)?;
    let authorization: StructuralCacheAuthorization =
        serde_json::from_slice(&bytes).context("invalid runtime structural cache authorization")?;
    ensure!(
        authorization.schema_version == STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION
            && authorization.offline_preprocessing
            && authorization.verify_digest(),
        "runtime structural-cache authorization contract is invalid"
    );
    let budget = &authorization.executed_operation_budget;
    let allowed_operations = budget
        .authorized_operations_by_language
        .get(language)
        .with_context(|| {
            format!("signed structural-cache operation admission lacks runtime language {language}")
        })?
        .iter()
        .cloned()
        .collect();
    Ok(Some(RuntimeStructuralCacheOperationAdmission {
        language: language.to_string(),
        allowed_operations,
        max_operations: usize::try_from(budget.max_operations)
            .context("signed structural-cache operation ceiling does not fit usize")?,
        signed_estimate: usize::try_from(budget.executed_estimate)
            .context("signed structural-cache operation estimate does not fit usize")?,
    }))
}

#[derive(Debug, Clone)]
pub struct IncrementalImpactPlan {
    pub executed_paths: BTreeSet<PathBuf>,
    pub inherited_paths: BTreeSet<PathBuf>,
    pub escalated_partitions: BTreeSet<String>,
    pub closure_edge_count: usize,
}

pub fn current_identity(
    repo_root: &Path,
    context_mode: BusinessContextMode,
) -> Result<StructuralCacheIdentity> {
    let repo = git2::Repository::discover(repo_root)
        .with_context(|| format!("{} is not a Git repository", repo_root.display()))?;
    let head = repo.head()?.peel_to_commit()?;
    let tree = head.tree()?;
    let report_identity =
        crate::lsp_completeness::current_report_identity(repo_root, context_mode)?;
    let root_slug = RootConfig::code_project(repo_root.to_path_buf()).slug();
    let tree_entries = git_tree_entries(&repo, &tree)?;
    let shared_influence_digest = influence_digest(&tree_entries, SHARED_INFLUENCE_PATTERNS);
    let partitions = partition_identities(&tree_entries)?;
    Ok(StructuralCacheIdentity {
        schema_version: STRUCTURAL_CACHE_IDENTITY_SCHEMA_VERSION,
        repository: report_identity.repository,
        commit: head.id().to_string(),
        tree: tree.id().to_string(),
        root_slug,
        configuration_digest: report_identity.config_digest,
        inventory_policy_digest: report_identity.policy_digest,
        context_mode: context_mode.to_string(),
        producer: current_producer_identity()?,
        shared_influence_digest,
        partitions,
    })
}

pub fn current_blob_ids(repo_root: &Path) -> Result<BTreeMap<String, String>> {
    let repo = git2::Repository::discover(repo_root)?;
    let head = repo.head()?.peel_to_commit()?;
    Ok(git_tree_entries(&repo, &head.tree()?)?
        .into_iter()
        .collect())
}

pub fn current_producer_identity() -> Result<StructuralProducerIdentity> {
    let executable = std::env::current_exe().context("resolve current RNA executable")?;
    let bytes = fs::read(&executable)
        .with_context(|| format!("read RNA executable {}", executable.display()))?;
    let producer_commit = env!("RNA_PRODUCER_COMMIT").to_string();
    if producer_commit.len() != 40 || !producer_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!(
            "RNA producer commit is unavailable; structural cache identity requires an exact 40-character Git commit"
        );
    }
    Ok(StructuralProducerIdentity {
        producer_commit,
        package_version: env!("CARGO_PKG_VERSION").to_string(),
        binary_sha256: hex_sha256(&bytes),
        graph_schema_version: crate::graph::store::SCHEMA_VERSION,
        graph_schema_signature: graph_schema_signature(),
        completeness_schema_version: crate::lsp_completeness::LSP_COMPLETENESS_SCHEMA_VERSION,
        work_item_schema_version: crate::extract::lsp::work_items::STORE_SCHEMA_VERSION,
        validation_evidence_schema_version:
            crate::extract::scan_stats::LSP_VALIDATION_EVIDENCE_SCHEMA_VERSION,
    })
}

pub fn authorization_path(repo_root: &Path) -> PathBuf {
    repo_root.join(STRUCTURAL_CACHE_AUTHORIZATION_PATH)
}

pub fn execution_path(repo_root: &Path) -> PathBuf {
    repo_root.join(STRUCTURAL_CACHE_EXECUTION_PATH)
}

fn verifier_authorized_executed_paths(
    authorization: &StructuralCacheAuthorization,
    inherited_by_path: &BTreeMap<String, InheritedFileAuthorization>,
) -> BTreeSet<PathBuf> {
    let mut executed = authorization
        .path_partitions
        .keys()
        .filter(|path| !inherited_by_path.contains_key(path.as_str()))
        .map(PathBuf::from)
        .collect::<BTreeSet<_>>();
    executed.extend(
        authorization
            .changed_paths
            .iter()
            .chain(authorization.added_paths.iter())
            .chain(authorization.deleted_paths.iter())
            .chain(authorization.invalidated_paths.iter())
            .map(PathBuf::from),
    );
    for rename in &authorization.renamed_paths {
        executed.insert(PathBuf::from(&rename[0]));
        executed.insert(PathBuf::from(&rename[1]));
    }
    executed
}

fn validate_executed_operation_budget(
    authorization: &StructuralCacheAuthorization,
    inherited_by_path: &BTreeMap<String, InheritedFileAuthorization>,
) -> Result<()> {
    let budget = &authorization.executed_operation_budget;
    const KNOWN_OPERATIONS: &[&str] = &[
        "call_hierarchy",
        "references",
        "definitions",
        "implementations",
        "type_hierarchy",
        "document_symbols",
        "document_links",
    ];
    ensure!(
        budget.max_operations == MAX_INCREMENTAL_LSP_OPERATIONS as u64,
        "signed structural-cache operation ceiling does not match this producer"
    );
    ensure!(
        budget.basis == VERIFIED_OPERATION_BUDGET_BASIS,
        "signed structural-cache operation-budget basis is unsupported"
    );
    let authorized_languages = authorization
        .path_partitions
        .values()
        .collect::<BTreeSet<_>>();
    ensure!(
        budget
            .authorized_operations_by_language
            .keys()
            .collect::<BTreeSet<_>>()
            == authorized_languages,
        "signed structural-cache authorized operations do not cover every target language"
    );
    for (language, operations) in &budget.authorized_operations_by_language {
        let mut normalized_operations = operations.clone();
        normalized_operations.sort();
        normalized_operations.dedup();
        ensure!(
            authorized_languages.contains(language)
                && normalized_operations == *operations
                && normalized_operations
                    .iter()
                    .all(|operation| KNOWN_OPERATIONS.contains(&operation.as_str())),
            "signed structural-cache authorized operations are invalid"
        );
    }
    let executed_paths = verifier_authorized_executed_paths(authorization, inherited_by_path);
    ensure!(
        budget.estimated_file_count <= executed_paths.len() as u64,
        "signed structural-cache operation estimate names more unseen files than the authorized execution set"
    );
    ensure!(
        budget.executed_estimate <= budget.max_operations,
        "signed structural-cache operation estimate exceeds its bound (max {} operations)",
        budget.max_operations
    );
    usize::try_from(budget.executed_estimate)
        .context("signed structural-cache operation estimate does not fit usize")?;
    Ok(())
}

fn retained_output_touching_executed_path(
    record: &crate::extract::lsp::work_items::LspWorkItemRecord,
    root_slug: &str,
    executed_paths: &BTreeSet<PathBuf>,
) -> Option<String> {
    if let Some(node) = record
        .output_nodes
        .iter()
        .find(|node| node.id.root == root_slug && executed_paths.contains(&node.id.file))
    {
        return Some(format!(
            "typed output node {} touches {}",
            node.stable_id(),
            node.id.file.display()
        ));
    }
    for edge in &record.output_edges {
        if edge.from.root == root_slug && executed_paths.contains(&edge.from.file) {
            return Some(format!(
                "typed output edge {} touches from path {}",
                edge.stable_id(),
                edge.from.file.display()
            ));
        }
        if edge.to.root == root_slug && executed_paths.contains(&edge.to.file) {
            return Some(format!(
                "typed output edge {} touches to path {}",
                edge.stable_id(),
                edge.to.file.display()
            ));
        }
    }
    None
}

pub fn load_verified_authorization(
    repo_root: &Path,
) -> Result<Option<VerifiedStructuralCacheAuthorization>> {
    let path = authorization_path(repo_root);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path)
        .with_context(|| format!("read structural cache authorization {}", path.display()))?;
    let expected_authorization_sha256 = std::env::var(STRUCTURAL_CACHE_AUTHORIZATION_SHA256_ENV)
        .with_context(|| {
            format!(
                "{} must bind the verifier-created structural cache authorization",
                STRUCTURAL_CACHE_AUTHORIZATION_SHA256_ENV
            )
        })?;
    require_sha256(
        &expected_authorization_sha256,
        "structural cache authorization handoff",
    )?;
    if hex_sha256(&bytes) != expected_authorization_sha256 {
        bail!("structural cache authorization differs from verifier handoff");
    }
    let authorization: StructuralCacheAuthorization = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid structural cache authorization {}", path.display()))?;
    if authorization.schema_version != STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION {
        bail!(
            "structural cache authorization schema {} does not match {}",
            authorization.schema_version,
            STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION
        );
    }
    if !authorization.offline_preprocessing {
        bail!("structural cache authorization is not marked offline preprocessing");
    }
    if !authorization.verify_digest() {
        bail!("structural cache authorization digest mismatch");
    }
    let identity = current_identity(repo_root, BusinessContextMode::Disabled)?;
    if authorization.repository != identity.repository
        || authorization.target_commit != identity.commit
        || authorization.target_tree != identity.tree
        || authorization.root_slug != identity.root_slug
        || authorization.producer != identity.producer
        || authorization.configuration_digest != identity.configuration_digest
    {
        bail!("structural cache authorization identity does not match target checkout/binary");
    }
    let expected_flags = QUALIFICATION_SCAN_FLAGS
        .iter()
        .map(|flag| (*flag).to_string())
        .collect::<Vec<_>>();
    if authorization.scan_flags != expected_flags {
        bail!("structural cache authorization scan flags mismatch");
    }
    for digest in [
        &authorization.toolchain_lock_digest,
        &authorization.inventory_digest,
        &authorization.inventory_file_sha256,
        &authorization.base_archive_sha256,
        &authorization.base_sidecar_sha256,
        &authorization.base_core_sha256,
        &authorization.base_report_sha256,
    ] {
        require_sha256(digest, "structural cache authorization digest")?;
    }

    let base_report = crate::lsp_completeness::load_report(repo_root)
        .context("injected structural cache is missing its base completeness report")?;
    if base_report.digest != authorization.base_report_digest
        || !base_report.integrity_violations().is_empty()
        || !base_report.is_ready()
    {
        bail!("injected base completeness report is not verifier-clean READY evidence");
    }
    let report_bytes = fs::read(crate::lsp_completeness::report_path(repo_root))?;
    if hex_sha256(&report_bytes) != authorization.base_report_sha256 {
        bail!("injected base completeness report SHA-256 mismatch");
    }
    if base_report.identity.checkout_sha != authorization.base_commit {
        bail!("base completeness report checkout does not match authorization");
    }
    let mut base_files = BTreeMap::new();
    for file in &base_report.files {
        if base_files.insert(file.path.as_str(), file).is_some() {
            bail!("injected base completeness report contains duplicate file evidence");
        }
    }
    let completed_records = crate::extract::lsp::work_items::load_all_records(repo_root)?
        .into_iter()
        .filter(|record| {
            record.state == crate::extract::lsp::work_items::LspWorkItemState::Completed
        })
        .map(|record| (format!("{}:{}", record.job_id, record.item_id), record))
        .collect::<BTreeMap<_, _>>();
    let validation_result_producers = crate::lsp_completeness::validation_result_producers(
        &crate::server::EnrichmentJobLedger::default().all_jobs(repo_root),
    );
    let execution = load_execution(repo_root)?;
    let mut executed_paths = BTreeSet::new();
    let mut executed_producer_ids = BTreeSet::new();
    if let Some(execution) = &execution {
        if execution.base_archive_sha256 != authorization.base_archive_sha256
            || execution.base_sidecar_sha256 != authorization.base_sidecar_sha256
            || execution.base_report_digest != authorization.base_report_digest
            || execution.target_commit != authorization.target_commit
            || execution.target_tree != authorization.target_tree
        {
            bail!("structural cache execution evidence does not match authorization");
        }
        for path in &execution.executed_paths {
            let normalized = crate::lsp_completeness::normalize_repo_relative_path(path)
                .map_err(anyhow::Error::msg)?;
            if normalized != *path {
                bail!("structural cache execution path is not normalized: {path}");
            }
            executed_paths.insert(path.clone());
        }
        executed_producer_ids.extend(execution.executed_producer_work_ids.iter().cloned());
        let mut operation_count = 0_u64;
        for producer_id in &executed_producer_ids {
            let record = completed_records.get(producer_id).with_context(|| {
                format!("execution evidence names missing producer {producer_id}")
            })?;
            if !executed_paths.contains(&record.file) {
                bail!("execution producer {producer_id} is outside the executed path set");
            }
            operation_count += record.requested_operations.len() as u64;
        }
        if operation_count != execution.executed_graph_enrichment_operation_count {
            bail!("structural cache executed work count/producer identity mismatch");
        }
    }

    let repo = git2::Repository::discover(repo_root)?;
    let base_commit = repo.find_commit(git2::Oid::from_str(&authorization.base_commit)?)?;
    if base_commit.tree_id().to_string() != authorization.base_tree {
        bail!("structural cache base commit/tree identity mismatch");
    }
    let target_tree = repo.find_tree(git2::Oid::from_str(&authorization.target_tree)?)?;
    let blobs = git_tree_entries(&repo, &target_tree)?;
    let blob_by_path = blobs.into_iter().collect::<BTreeMap<_, _>>();
    for (path, language) in &authorization.path_partitions {
        let normalized = crate::lsp_completeness::normalize_repo_relative_path(path)
            .map_err(anyhow::Error::msg)?;
        if normalized != *path
            || !blob_by_path.contains_key(path)
            || !identity.partitions.contains_key(language)
        {
            bail!("invalid structural cache path partition binding: {path} -> {language}");
        }
    }
    let mut inherited_by_path = BTreeMap::new();
    for mut file in authorization.inherited_files.clone() {
        normalize_authorized_file(&mut file)?;
        if inherited_by_path.contains_key(&file.path) {
            bail!("duplicate inherited file authorization for {}", file.path);
        }
        let actual_blob = blob_by_path
            .get(&file.path)
            .with_context(|| format!("inherited path is absent from target tree: {}", file.path))?;
        if actual_blob != &file.blob {
            bail!("inherited blob mismatch for {}", file.path);
        }
        let base_file = base_files
            .get(file.path.as_str())
            .with_context(|| format!("base report has no file evidence for {}", file.path))?;
        if canonical_json_sha256(*base_file)? != file.base_file_sha256 {
            bail!(
                "inherited base file evidence digest mismatch for {}",
                file.path
            );
        }
        let base_result_ids = base_file
            .expected_result_ids
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        if base_result_ids != file.expected_result_ids
            || base_file.language.as_deref() != Some(file.language.as_str())
        {
            bail!(
                "inherited base file/result identity mismatch for {}",
                file.path
            );
        }
        let lineage_result_ids = file
            .result_producers
            .iter()
            .map(|lineage| lineage.result_id.clone())
            .collect::<Vec<_>>();
        if lineage_result_ids != file.expected_result_ids {
            bail!("inherited result lineage is incomplete for {}", file.path);
        }
        let file_was_executed = executed_paths.contains(&file.path);
        let records_for_file = if file_was_executed {
            Vec::new()
        } else {
            completed_records
                .iter()
                .filter(|(_, record)| record.file == file.path)
                .collect::<Vec<_>>()
        };
        let producer_work_ids = records_for_file
            .iter()
            .map(|(producer_id, _)| (*producer_id).clone())
            .collect::<Vec<_>>();
        let input_hashes = records_for_file
            .iter()
            .filter_map(|(_, record)| {
                (!record.input_hash.is_empty()).then_some(record.input_hash.clone())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let operations = records_for_file
            .iter()
            .flat_map(|(_, record)| record.requested_operations.iter().cloned())
            .chain(
                base_file
                    .requests_attempted
                    .iter()
                    .map(|request| request.method.clone()),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let producer_graph_enrichment_operation_count = records_for_file
            .iter()
            .map(|(_, record)| record.requested_operations.len() as u64)
            .sum::<u64>();
        if !file_was_executed
            && (producer_work_ids != file.producer_work_ids
                || producer_graph_enrichment_operation_count
                    != file.producer_graph_enrichment_operation_count
                || input_hashes != file.input_hashes
                || operations != file.operations)
        {
            bail!(
                "inherited work input/operation identity mismatch for {}",
                file.path
            );
        }
        if !file_was_executed {
            for lineage in &file.result_producers {
                for producer_id in &lineage.producer_ids {
                    if validation_result_producers
                        .get(&(file.path.clone(), lineage.result_id.clone()))
                        .is_some_and(|producers| producers.contains(producer_id))
                    {
                        continue;
                    }
                    let record = completed_records.get(producer_id).with_context(|| {
                        format!(
                            "inherited result {} names missing producer {}",
                            lineage.result_id, producer_id
                        )
                    })?;
                    if record.file != file.path
                        || !record.produced_result_ids.contains(&lineage.result_id)
                    {
                        bail!(
                            "inherited result {} is not emitted by producer {}",
                            lineage.result_id,
                            producer_id
                        );
                    }
                }
            }
        }
        let partition = identity
            .partitions
            .get(&file.language)
            .with_context(|| format!("missing current partition for {}", file.language))?;
        if partition.signature != file.partition_signature {
            bail!("inherited partition signature mismatch for {}", file.path);
        }
        if authorization
            .invalidated_partitions
            .iter()
            .any(|language| language == &file.language)
        {
            bail!(
                "invalidated partition cannot authorize inheritance: {}",
                file.language
            );
        }
        inherited_by_path.insert(file.path.clone(), file);
    }
    let mut authorized_current_producers = executed_producer_ids;
    for file in inherited_by_path.values() {
        if !executed_paths.contains(&file.path) {
            authorized_current_producers.extend(file.producer_work_ids.iter().cloned());
        }
    }
    if completed_records.keys().cloned().collect::<BTreeSet<_>>() != authorized_current_producers {
        bail!("structural cache work ledger contains unauthenticated producer records");
    }
    let planned_executed_paths =
        verifier_authorized_executed_paths(&authorization, &inherited_by_path);
    for (producer_id, record) in completed_records.iter().filter(|(_, record)| {
        inherited_by_path.contains_key(&record.file) && !executed_paths.contains(&record.file)
    }) {
        if let Some(detail) = retained_output_touching_executed_path(
            record,
            &authorization.root_slug,
            &planned_executed_paths,
        ) {
            bail!(
                "retained structural-cache producer {producer_id} crosses the signed execution path set: {detail}"
            );
        }
    }
    validate_executed_operation_budget(&authorization, &inherited_by_path)?;
    attest_runtime_authorization(repo_root, &expected_authorization_sha256)?;
    Ok(Some(VerifiedStructuralCacheAuthorization {
        authorization,
        inherited_by_path,
        base_report,
    }))
}

pub fn plan_incremental_impact(
    authorization: &VerifiedStructuralCacheAuthorization,
    old_nodes: &[Node],
    old_edges: &[Edge],
    new_nodes: &[Node],
    _new_edges: &[Edge],
) -> IncrementalImpactPlan {
    let target_paths = authorization
        .authorization
        .path_partitions
        .keys()
        .map(PathBuf::from)
        .collect::<BTreeSet<_>>();
    let mut direct_seeds = authorization
        .authorization
        .changed_paths
        .iter()
        .chain(authorization.authorization.added_paths.iter())
        .chain(authorization.authorization.deleted_paths.iter())
        .map(PathBuf::from)
        .collect::<BTreeSet<_>>();
    for rename in &authorization.authorization.renamed_paths {
        direct_seeds.insert(PathBuf::from(&rename[0]));
        direct_seeds.insert(PathBuf::from(&rename[1]));
    }
    let mut executed = direct_seeds.clone();
    // The verifier authorizes every target path that is not backed by an
    // inherited file record. Some of those paths are neither a direct tree
    // delta nor a partition invalidation (for example a base cache that never
    // produced reusable evidence for that file), so seed the runtime plan from
    // the complete signed execution set rather than relying on a later broad
    // operation-bound escalation to discover them.
    executed.extend(verifier_authorized_executed_paths(
        &authorization.authorization,
        &authorization.inherited_by_path,
    ));
    executed.extend(
        authorization
            .authorization
            .invalidated_paths
            .iter()
            .map(PathBuf::from),
    );

    let invalidated = authorization
        .authorization
        .invalidated_partitions
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    for (path, language) in &authorization.authorization.path_partitions {
        if invalidated.contains(language) {
            executed.insert(PathBuf::from(path));
        }
    }

    let root_slug = authorization.authorization.root_slug.as_str();
    let limit = MAX_IMPACT_FILES.min(
        target_paths
            .len()
            .saturating_mul(MAX_IMPACT_FRACTION_NUMERATOR)
            .checked_div(MAX_IMPACT_FRACTION_DENOMINATOR)
            .unwrap_or_default()
            .max(1),
    );
    let mut escalated = BTreeSet::new();
    let mut closure_edge_ids = BTreeSet::new();

    // The verifier independently signs this same fixed-point path set. Propagate
    // only persisted LSP impact relations whose endpoints are expressible as
    // target-inventory paths in this root. External and pathless hubs therefore
    // cannot turn a local update into an unbounded repository traversal.
    //
    // Bounds retain their existing partition-escalation semantics. Escalation
    // itself may expose a crossing carried edge, so closure and both bounds are
    // recomputed until neither the execution set nor the escalated partitions
    // can grow further.
    loop {
        let executed_before = executed.len();
        let escalated_before = escalated.len();

        expand_persisted_lsp_impact_closure(
            root_slug,
            &target_paths,
            old_edges,
            &mut executed,
            &mut closure_edge_ids,
        );

        if executed.len() > limit {
            for path in &executed {
                if let Some(language) = authorization
                    .authorization
                    .path_partitions
                    .get(path.to_string_lossy().as_ref())
                {
                    escalated.insert(language.clone());
                }
            }
            for node in old_nodes.iter().chain(new_nodes.iter()) {
                if node.id.root == root_slug && executed.contains(&node.id.file) {
                    escalated.insert(node.language.clone());
                }
            }
        }

        // The verifier signs the capability-aware shared-server estimate from
        // completed per-path work. Recomputing a static operation profile for
        // every node invents unsupported work (for example typeHierarchy) and
        // resets the shared budget once per symbol. Invalid or over-limit
        // authorization escalates fail-closed; the exact handoff rejects that
        // unsigned expansion before an LSP request is made.
        if !matches!(
            authorization.signed_executed_operation_count(),
            Ok(count) if count <= MAX_INCREMENTAL_LSP_OPERATIONS
        ) {
            escalated.extend(
                authorization
                    .authorization
                    .path_partitions
                    .iter()
                    .filter(|(path, _)| executed.contains(Path::new(path.as_str())))
                    .map(|(_, language)| language.clone()),
            );
        }

        for (path, language) in &authorization.authorization.path_partitions {
            if escalated.contains(language) {
                executed.insert(PathBuf::from(path));
            }
        }

        if executed.len() == executed_before && escalated.len() == escalated_before {
            break;
        }
    }

    let inherited = authorization
        .inherited_by_path
        .keys()
        .map(PathBuf::from)
        .filter(|path| !executed.contains(path))
        .collect::<BTreeSet<_>>();
    IncrementalImpactPlan {
        executed_paths: executed,
        inherited_paths: inherited,
        escalated_partitions: escalated,
        closure_edge_count: closure_edge_ids.len(),
    }
}

fn expand_persisted_lsp_impact_closure(
    root_slug: &str,
    target_paths: &BTreeSet<PathBuf>,
    old_edges: &[Edge],
    executed: &mut BTreeSet<PathBuf>,
    closure_edge_ids: &mut BTreeSet<String>,
) {
    loop {
        let mut additions = BTreeSet::new();
        for edge in old_edges {
            if edge.source != crate::graph::ExtractionSource::Lsp
                || !is_impact_edge(&edge.kind)
                || edge.from.file == edge.to.file
                || edge.from.file.as_os_str().is_empty()
                || edge.to.file.as_os_str().is_empty()
            {
                continue;
            }
            let from_is_executed =
                edge.from.root == root_slug && executed.contains(&edge.from.file);
            let to_is_executed = edge.to.root == root_slug && executed.contains(&edge.to.file);
            let from_is_target =
                edge.from.root == root_slug && target_paths.contains(&edge.from.file);
            let to_is_target = edge.to.root == root_slug && target_paths.contains(&edge.to.file);
            if (from_is_executed && to_is_target) || (to_is_executed && from_is_target) {
                closure_edge_ids.insert(edge.stable_id());
            }
            if from_is_executed && to_is_target {
                additions.insert(edge.to.file.clone());
            }
            if to_is_executed && from_is_target {
                additions.insert(edge.from.file.clone());
            }
        }
        let before = executed.len();
        executed.extend(additions);
        if executed.len() == before {
            break;
        }
    }
}

pub fn validate_runtime_plan_handoff(
    authorization: &VerifiedStructuralCacheAuthorization,
    plan: &IncrementalImpactPlan,
) -> Result<()> {
    let expected_executed = verifier_authorized_executed_paths(
        &authorization.authorization,
        &authorization.inherited_by_path,
    );

    if plan.executed_paths != expected_executed {
        let first_unexpected = plan
            .executed_paths
            .difference(&expected_executed)
            .next()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "-".to_string());
        let first_missing = expected_executed
            .difference(&plan.executed_paths)
            .next()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "-".to_string());
        bail!(
            "runtime structural-cache execution differs from verifier authorization: expected={} actual={} first_unexpected={} first_missing={}",
            expected_executed.len(),
            plan.executed_paths.len(),
            first_unexpected,
            first_missing,
        );
    }

    let expected_inherited = authorization
        .inherited_by_path
        .keys()
        .map(PathBuf::from)
        .filter(|path| !expected_executed.contains(path))
        .collect::<BTreeSet<_>>();
    if plan.inherited_paths != expected_inherited {
        let first_unexpected = plan
            .inherited_paths
            .difference(&expected_inherited)
            .next()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "-".to_string());
        let first_missing = expected_inherited
            .difference(&plan.inherited_paths)
            .next()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "-".to_string());
        bail!(
            "runtime structural-cache inheritance differs from verifier authorization: expected={} actual={} first_unexpected={} first_missing={}",
            expected_inherited.len(),
            plan.inherited_paths.len(),
            first_unexpected,
            first_missing,
        );
    }

    Ok(())
}

pub fn write_execution(repo_root: &Path, execution: &StructuralCacheExecution) -> Result<()> {
    let mut execution = execution.clone();
    execution.finalize();
    let path = execution_path(repo_root);
    let parent = path
        .parent()
        .context("cache execution path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".structural-cache-execution.tmp-{}",
        std::process::id()
    ));
    let mut bytes = serde_json::to_vec_pretty(&execution)?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, &path)?;
    Ok(())
}

pub fn build_execution(
    repo_root: &Path,
    authorization: &VerifiedStructuralCacheAuthorization,
    plan: &IncrementalImpactPlan,
    validations: &[crate::extract::scan_stats::LspValidationEvidence],
    execution_job_id: Option<String>,
    scan_started_at_ms: u64,
) -> Result<StructuralCacheExecution> {
    let changed_paths = authorization
        .authorization
        .changed_paths
        .iter()
        .chain(authorization.authorization.added_paths.iter())
        .chain(authorization.authorization.deleted_paths.iter())
        .map(PathBuf::from)
        .chain(
            authorization
                .authorization
                .renamed_paths
                .iter()
                .flat_map(|rename| [PathBuf::from(&rename[0]), PathBuf::from(&rename[1])]),
        )
        .collect::<BTreeSet<_>>();
    let base_producers = authorization
        .inherited_by_path
        .values()
        .flat_map(|file| file.producer_work_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let executed_records = select_execution_work_items(
        crate::extract::lsp::work_items::load_all_records(repo_root)?,
        &plan.executed_paths,
        &base_producers,
        scan_started_at_ms,
    );
    let executed_graph_enrichment_operation_count = executed_records
        .iter()
        .map(|record| record.requested_operations.len() as u64)
        .sum();
    ensure!(
        executed_graph_enrichment_operation_count <= MAX_INCREMENTAL_LSP_OPERATIONS as u64,
        "executed structural-cache operation count exceeds its signed producer bound"
    );
    let executed_producer_work_ids = executed_records
        .iter()
        .map(|record| format!("{}:{}", record.job_id, record.item_id))
        .collect();

    Ok(StructuralCacheExecution {
        schema_version: STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION,
        offline_preprocessing: true,
        base_archive_sha256: authorization.authorization.base_archive_sha256.clone(),
        base_sidecar_sha256: authorization.authorization.base_sidecar_sha256.clone(),
        base_report_digest: authorization.authorization.base_report_digest.clone(),
        target_commit: authorization.authorization.target_commit.clone(),
        target_tree: authorization.authorization.target_tree.clone(),
        inherited_paths: plan
            .inherited_paths
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
        executed_paths: plan
            .executed_paths
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
        invalidated_partitions: authorization.authorization.invalidated_partitions.clone(),
        escalated_partitions: plan.escalated_partitions.iter().cloned().collect(),
        changed_file_count: changed_paths.len() as u64,
        invalidated_file_count: plan.executed_paths.difference(&changed_paths).count() as u64,
        inherited_graph_enrichment_operation_count: plan
            .inherited_paths
            .iter()
            .filter_map(|path| {
                authorization
                    .inherited_by_path
                    .get(path.to_string_lossy().as_ref())
            })
            .map(|file| file.producer_graph_enrichment_operation_count)
            .sum(),
        executed_graph_enrichment_operation_count,
        inherited_readiness_validation_request_count: authorization
            .inherited_readiness_validation_request_count(&plan.inherited_paths)?,
        executed_readiness_validation_request_count: validations
            .iter()
            .filter(|validation| validation.method.is_some())
            .count() as u64,
        executed_producer_work_ids,
        closure_edge_count: plan.closure_edge_count as u64,
        execution_job_id,
        digest: String::new(),
    })
}

fn select_execution_work_items(
    records: Vec<crate::extract::lsp::work_items::LspWorkItemRecord>,
    executed_paths: &BTreeSet<PathBuf>,
    base_producers: &BTreeSet<String>,
    scan_started_at_ms: u64,
) -> Vec<crate::extract::lsp::work_items::LspWorkItemRecord> {
    records
        .into_iter()
        .filter(|record| {
            executed_paths.contains(Path::new(&record.file))
                && !base_producers.contains(&format!("{}:{}", record.job_id, record.item_id))
                && record.updated_at_ms >= scan_started_at_ms
                && record.state == crate::extract::lsp::work_items::LspWorkItemState::Completed
        })
        .collect()
}

pub fn execution_related_job_ids(execution: &StructuralCacheExecution) -> Result<Vec<String>> {
    let mut related = execution
        .execution_job_id
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    for producer_id in &execution.executed_producer_work_ids {
        let (job_id, item_id) = producer_id
            .rsplit_once(':')
            .with_context(|| format!("invalid structural-cache producer ID {producer_id}"))?;
        item_id
            .parse::<usize>()
            .with_context(|| format!("invalid structural-cache producer ID {producer_id}"))?;
        if job_id.is_empty() {
            bail!("invalid structural-cache producer ID {producer_id}");
        }
        related.insert(job_id.to_string());
    }
    Ok(related.into_iter().collect())
}

pub fn load_execution(repo_root: &Path) -> Result<Option<StructuralCacheExecution>> {
    let path = execution_path(repo_root);
    if !path.is_file() {
        return Ok(None);
    }
    let execution: StructuralCacheExecution = serde_json::from_slice(&fs::read(&path)?)?;
    if execution.schema_version != STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION
        || !execution.offline_preprocessing
        || !execution.verify_digest()
    {
        bail!("invalid structural cache execution evidence");
    }
    Ok(Some(execution))
}

pub async fn validate_persisted_target(
    repo_root: &Path,
    expected_nodes: &[Node],
    expected_edges: &[Edge],
    stale_paths: &BTreeSet<PathBuf>,
) -> Result<()> {
    let reloaded = crate::server::store::load_graph_from_lance(repo_root)
        .await
        .context("reopen persisted structural cache")?;
    // `language` is intentionally not a symbols-table column: ordinary nodes
    // recover it from `file_path`, while repository/module sentinels with an
    // empty path reopen as `unknown`. Per-file readiness and the cache
    // authorization bind the language partition separately, so compare the
    // exact persisted projection rather than a derived in-memory field.
    let mut expected_persisted_nodes = expected_nodes.to_vec();
    let mut actual_persisted_nodes = reloaded.nodes.clone();
    for node in expected_persisted_nodes
        .iter_mut()
        .chain(actual_persisted_nodes.iter_mut())
    {
        node.language.clear();
    }
    let expected_node_digest = graph_records_digest(&expected_persisted_nodes, Node::stable_id)?;
    let actual_node_digest = graph_records_digest(&actual_persisted_nodes, Node::stable_id)?;
    let expected_edge_digest = graph_records_digest(expected_edges, Edge::stable_id)?;
    let actual_edge_digest = graph_records_digest(&reloaded.edges, Edge::stable_id)?;
    if expected_node_digest != actual_node_digest || expected_edge_digest != actual_edge_digest {
        let node_difference = first_graph_record_difference(
            &expected_persisted_nodes,
            &actual_persisted_nodes,
            Node::stable_id,
        )?;
        let edge_difference =
            first_graph_record_difference(expected_edges, &reloaded.edges, Edge::stable_id)?;
        bail!(
            "persisted structural cache differs after Lance reopen: nodes expected={} actual={}, edges expected={} actual={}; node difference={}; edge difference={}",
            expected_nodes.len(),
            reloaded.nodes.len(),
            expected_edges.len(),
            reloaded.edges.len(),
            node_difference.unwrap_or_else(|| "none".to_string()),
            edge_difference.unwrap_or_else(|| "none".to_string()),
        );
    }
    if reloaded
        .nodes
        .iter()
        .any(|node| stale_paths.contains(&node.id.file))
        || reloaded.edges.iter().any(|edge| {
            stale_paths.contains(&edge.from.file) || stale_paths.contains(&edge.to.file)
        })
    {
        bail!("persisted structural cache retains a deleted/old-rename path");
    }
    Ok(())
}

fn graph_schema_signature() -> String {
    let mut fields = Vec::new();
    for (table, schema) in [
        ("symbols", crate::graph::store::symbols_schema()),
        ("edges", crate::graph::store::edges_schema()),
    ] {
        for field in schema.fields() {
            fields.push(format!(
                "{table}:{}:{:?}:{}",
                field.name(),
                field.data_type(),
                field.is_nullable()
            ));
        }
    }
    fields.sort();
    hex_sha256(fields.join("\n").as_bytes())
}

fn partition_identities(
    tree_entries: &[(String, String)],
) -> Result<BTreeMap<String, LanguagePartitionIdentity>> {
    let mut partitions = BTreeMap::new();
    for descriptor in crate::extract::lsp::builtin_lsp_descriptors() {
        let identity = descriptor.partition_identity();
        let descriptor_bytes = serde_json::to_vec(&identity)?;
        let descriptor_signature = hex_sha256(&descriptor_bytes);
        let patterns = descriptor
            .partition_influence_patterns()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let influence_digest = influence_digest_owned(tree_entries, &patterns);
        let signature =
            hex_sha256(format!("{descriptor_signature}\0{influence_digest}").as_bytes());
        let matched_file_count = tree_entries
            .iter()
            .filter(|(path, _)| {
                patterns.iter().any(|pattern| {
                    crate::extract::lsp::partition_influence_pattern_matches(pattern, path)
                })
            })
            .count() as u64;
        let language = descriptor.language().to_string();
        let partition = LanguagePartitionIdentity {
            language: language.clone(),
            descriptor_signature,
            influence_patterns: patterns,
            influence_digest,
            signature,
            matched_file_count,
        };
        if partitions.insert(language.clone(), partition).is_some() {
            bail!("duplicate structural cache language partition: {language}");
        }
    }
    Ok(partitions)
}

fn influence_digest(tree_entries: &[(String, String)], patterns: &[&str]) -> String {
    let owned = patterns
        .iter()
        .map(|pattern| (*pattern).to_string())
        .collect::<Vec<_>>();
    influence_digest_owned(tree_entries, &owned)
}

fn influence_digest_owned(tree_entries: &[(String, String)], patterns: &[String]) -> String {
    let mut selected = tree_entries
        .iter()
        .filter(|(path, _)| {
            patterns.iter().any(|pattern| {
                crate::extract::lsp::partition_influence_pattern_matches(pattern, path)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    selected.sort();
    hex_sha256(&serde_json::to_vec(&selected).expect("tree influence serialization cannot fail"))
}

fn git_tree_entries(
    _repo: &git2::Repository,
    tree: &git2::Tree<'_>,
) -> Result<Vec<(String, String)>> {
    let mut entries = Vec::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |prefix, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob)
            && let Ok(name) = entry.name()
        {
            entries.push((format!("{prefix}{name}"), entry.id().to_string()));
        }
        git2::TreeWalkResult::Ok
    })?;
    entries.sort();
    Ok(entries)
}

fn normalize_authorized_file(file: &mut InheritedFileAuthorization) -> Result<()> {
    let normalized = crate::lsp_completeness::normalize_repo_relative_path(&file.path)
        .map_err(anyhow::Error::msg)?;
    if normalized != file.path {
        bail!("inherited file path is not normalized: {}", file.path);
    }
    require_git_oid(&file.blob, "inherited blob")?;
    require_sha256(&file.partition_signature, "partition signature")?;
    require_sha256(&file.base_file_sha256, "base file evidence")?;
    file.input_hashes.sort();
    file.input_hashes.dedup();
    file.operations.sort();
    file.operations.dedup();
    file.producer_work_ids.sort();
    file.producer_work_ids.dedup();
    file.expected_result_ids.sort();
    file.expected_result_ids.dedup();
    file.result_producers
        .sort_by(|left, right| left.result_id.cmp(&right.result_id));
    for producer in &mut file.result_producers {
        producer.producer_ids.sort();
        producer.producer_ids.dedup();
        if producer.result_id.is_empty() || producer.producer_ids.is_empty() {
            bail!("inherited result lineage must name a result and producer");
        }
    }
    Ok(())
}

fn is_impact_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls
            | EdgeKind::ReferencedBy
            | EdgeKind::References
            | EdgeKind::DependsOn
            | EdgeKind::Implements
            | EdgeKind::ReExports
            | EdgeKind::TestedBy
    )
}

fn require_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} must be a 64-character hexadecimal SHA-256");
    }
    Ok(())
}

fn require_git_oid(value: &str, label: &str) -> Result<()> {
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} must be a 40-character hexadecimal Git object ID");
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn canonical_json_sha256<T: Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    let mut output = String::new();
    write_canonical_json_value(&value, &mut output)?;
    output.push('\n');
    Ok(hex_sha256(output.as_bytes()))
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
            entries.sort_by(|left, right| left.0.cmp(right.0));
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

fn graph_records_digest<T, F>(records: &[T], stable_id: F) -> Result<String>
where
    T: Serialize,
    F: Fn(&T) -> String,
{
    let mut ordered = records
        .iter()
        .map(|record| Ok((stable_id(record), serde_json::to_value(record)?)))
        .collect::<Result<Vec<_>>>()?;
    ordered.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.to_string().cmp(&right.1.to_string()))
    });
    canonical_json_sha256(&ordered)
}

fn first_graph_record_difference<T, F>(
    expected: &[T],
    actual: &[T],
    stable_id: F,
) -> Result<Option<String>>
where
    T: Serialize,
    F: Fn(&T) -> String,
{
    fn records_by_id<T, F>(records: &[T], stable_id: &F) -> Result<BTreeMap<String, Vec<String>>>
    where
        T: Serialize,
        F: Fn(&T) -> String,
    {
        let mut by_id = BTreeMap::<String, Vec<String>>::new();
        for record in records {
            by_id
                .entry(stable_id(record))
                .or_default()
                .push(serde_json::to_string(record)?);
        }
        for records in by_id.values_mut() {
            records.sort();
        }
        Ok(by_id)
    }

    let expected = records_by_id(expected, &stable_id)?;
    let actual = records_by_id(actual, &stable_id)?;
    for id in expected
        .keys()
        .chain(actual.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
    {
        if expected.get(&id) != actual.get(&id) {
            let mut detail = format!(
                "id={id} expected={:?} actual={:?}",
                expected.get(&id),
                actual.get(&id)
            );
            detail.truncate(2_000);
            return Ok(Some(detail));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_work_selection_excludes_stale_same_path_history() {
        let record = |job_id: &str, item_id, updated_at_ms| {
            crate::extract::lsp::work_items::LspWorkItemRecord {
                job_id: job_id.to_string(),
                item_id,
                file: "src/app.py".to_string(),
                requested_operations: vec!["references".to_string()],
                state: crate::extract::lsp::work_items::LspWorkItemState::Completed,
                updated_at_ms,
                ..Default::default()
            }
        };
        let selected = select_execution_work_items(
            vec![
                record("stale-pass1", 0, 9_999),
                record("current-pass1", 0, 10_000),
            ],
            &BTreeSet::from([PathBuf::from("src/app.py")]),
            &BTreeSet::new(),
            10_000,
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].job_id, "current-pass1");
    }

    #[test]
    fn signed_runtime_marker_requires_its_authorization_file() {
        assert!(!validate_runtime_authorization_presence(false, false).unwrap());
        assert!(validate_runtime_authorization_presence(true, true).unwrap());
        let error = validate_runtime_authorization_presence(false, true).unwrap_err();
        assert!(error.to_string().contains("authorization file is missing"));
    }
    use crate::graph::{Confidence, ExtractionSource, NodeId, NodeKind};

    fn node(path: &str, language: &str) -> Node {
        Node {
            id: NodeId {
                root: "fixture".to_string(),
                file: PathBuf::from(path),
                name: path.to_string(),
                kind: NodeKind::Function,
            },
            language: language.to_string(),
            line_start: 1,
            line_end: 1,
            signature: format!("fn {path}"),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn edge(from: &Node, to: &Node) -> Edge {
        Edge {
            from: from.id.clone(),
            to: to.id.clone(),
            kind: EdgeKind::ReferencedBy,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        }
    }

    fn verified_authorization(
        inherited: &[(&str, &str)],
        changed: &[&str],
        added: &[&str],
        deleted: &[&str],
        renamed: &[[&str; 2]],
        invalidated_partitions: &[&str],
        invalidated_paths: &[&str],
    ) -> VerifiedStructuralCacheAuthorization {
        let invalidated_path_set = invalidated_paths.iter().copied().collect::<BTreeSet<_>>();
        let invalidated_partition_set = invalidated_partitions
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let inherited_files = inherited
            .iter()
            .filter(|(path, language)| {
                !invalidated_path_set.contains(path)
                    && !invalidated_partition_set.contains(language)
            })
            .map(|(path, language)| InheritedFileAuthorization {
                path: (*path).to_string(),
                blob: "a".repeat(40),
                language: (*language).to_string(),
                partition_signature: "b".repeat(64),
                base_file_sha256: "c".repeat(64),
                input_hashes: Vec::new(),
                operations: Vec::new(),
                producer_work_ids: Vec::new(),
                producer_graph_enrichment_operation_count: 0,
                expected_result_ids: Vec::new(),
                result_producers: Vec::new(),
            })
            .collect::<Vec<_>>();
        let inherited_by_path = inherited_files
            .iter()
            .map(|file| (file.path.clone(), file.clone()))
            .collect();
        let authorization = StructuralCacheAuthorization {
            schema_version: STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION,
            offline_preprocessing: true,
            repository: "owner/repo".to_string(),
            base_commit: "d".repeat(40),
            base_tree: "e".repeat(40),
            target_commit: "f".repeat(40),
            target_tree: "1".repeat(40),
            root_slug: "fixture".to_string(),
            producer: StructuralProducerIdentity {
                producer_commit: "2".repeat(40),
                package_version: "1".to_string(),
                binary_sha256: "3".repeat(64),
                graph_schema_version: 24,
                graph_schema_signature: "4".repeat(64),
                completeness_schema_version: 5,
                work_item_schema_version: 4,
                validation_evidence_schema_version: 1,
            },
            toolchain_lock_digest: "5".repeat(64),
            inventory_digest: "6".repeat(64),
            inventory_file_sha256: "7".repeat(64),
            configuration_digest: "config".to_string(),
            scan_flags: QUALIFICATION_SCAN_FLAGS
                .iter()
                .map(|flag| (*flag).to_string())
                .collect(),
            base_archive_sha256: "8".repeat(64),
            base_sidecar_sha256: "9".repeat(64),
            base_core_sha256: "a".repeat(64),
            base_report_digest: "report".to_string(),
            base_report_sha256: "b".repeat(64),
            inherited_files,
            changed_paths: changed.iter().map(|path| (*path).to_string()).collect(),
            added_paths: added.iter().map(|path| (*path).to_string()).collect(),
            deleted_paths: deleted.iter().map(|path| (*path).to_string()).collect(),
            renamed_paths: renamed
                .iter()
                .map(|rename| [rename[0].to_string(), rename[1].to_string()])
                .collect(),
            invalidated_partitions: invalidated_partitions
                .iter()
                .map(|language| (*language).to_string())
                .collect(),
            invalidated_paths: invalidated_paths
                .iter()
                .map(|path| (*path).to_string())
                .collect(),
            path_partitions: inherited
                .iter()
                .map(|(path, language)| ((*path).to_string(), (*language).to_string()))
                .chain(
                    changed
                        .iter()
                        .map(|path| ((*path).to_string(), "python".to_string())),
                )
                .collect(),
            executed_operation_budget: StructuralCacheOperationBudget {
                max_operations: MAX_INCREMENTAL_LSP_OPERATIONS as u64,
                executed_estimate: changed.len() as u64,
                authorized_operations_by_language: inherited
                    .iter()
                    .map(|(_, language)| (*language).to_string())
                    .chain((!changed.is_empty()).then_some("python".to_string()))
                    .map(|language| {
                        (
                            language,
                            vec!["call_hierarchy".to_string(), "document_symbols".to_string()],
                        )
                    })
                    .collect(),
                basis: VERIFIED_OPERATION_BUDGET_BASIS.to_string(),
                estimated_file_count: 0,
            },
            digest: String::new(),
        };
        VerifiedStructuralCacheAuthorization {
            authorization,
            inherited_by_path,
            base_report: crate::lsp_completeness::LspCompletenessReport::new(
                crate::lsp_completeness::ReportIdentity::new(
                    "d".repeat(40),
                    "config",
                    "policy",
                    "disabled",
                    24,
                    "generation",
                ),
                Vec::new(),
            ),
        }
    }

    #[test]
    fn wildcard_patterns_cover_descriptor_owned_config_files() {
        assert!(crate::extract::lsp::partition_influence_pattern_matches(
            "requirements*.txt",
            "requirements-dev.txt"
        ));
        assert!(crate::extract::lsp::partition_influence_pattern_matches(
            "**/requirements*.txt",
            "env/requirements-test.txt"
        ));
        assert!(crate::extract::lsp::partition_influence_pattern_matches(
            "tsconfig.json",
            "client/tsconfig.json"
        ));
        assert!(!crate::extract::lsp::partition_influence_pattern_matches(
            "requirements*.txt",
            "src/main.py"
        ));
    }

    /// A dependency manifest under a directory that the pattern prefix names
    /// must still invalidate the partition. The basename of
    /// `requirements/dev.txt` is `dev.txt`, so only the whole-path fallback
    /// clause catches it — none of the cases above exercise that clause, which
    /// is how its removal went unnoticed. Losing this match is fail-open:
    /// dependency changes would stop invalidating the Python partition.
    #[test]
    fn wildcard_patterns_cover_manifests_under_a_matching_directory() {
        assert!(crate::extract::lsp::partition_influence_pattern_matches(
            "requirements*.txt",
            "requirements/dev.txt"
        ));
        // The fallback is prefix-anchored, not a substring search: the pattern
        // must match the whole path from its first byte. A manifest nested
        // below an unrelated directory is therefore still only reachable via
        // its basename, which is the same boundary the pre-consolidation
        // matcher had.
        assert!(!crate::extract::lsp::partition_influence_pattern_matches(
            "requirements*.txt",
            "deploy/requirements/base.txt"
        ));
        // And an unrelated path is not a match just because the whole path is
        // consulted.
        assert!(!crate::extract::lsp::partition_influence_pattern_matches(
            "requirements*.txt",
            "docs/notes.txt"
        ));
    }

    fn influence_entries(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(path, blob)| ((*path).to_string(), (*blob).to_string()))
            .collect()
    }

    fn changed_partition_signatures(
        base: &[(String, String)],
        target: &[(String, String)],
    ) -> BTreeSet<String> {
        let base = partition_identities(base).unwrap();
        let target = partition_identities(target).unwrap();
        base.into_iter()
            .filter_map(|(language, partition)| {
                (target[&language].signature != partition.signature).then_some(language)
            })
            .collect()
    }

    #[test]
    fn python_project_config_changes_only_python_owned_partitions() {
        let base = influence_entries(&[
            ("pyproject.toml", "base-pyproject"),
            ("setup.cfg", "base-setup-cfg"),
            ("setup.py", "base-setup-py"),
            ("tox.ini", "base-tox"),
        ]);
        let target = influence_entries(&[
            ("pyproject.toml", "target-pyproject"),
            ("setup.cfg", "target-setup-cfg"),
            ("setup.py", "target-setup-py"),
            ("tox.ini", "target-tox"),
        ]);

        assert_eq!(
            changed_partition_signatures(&base, &target),
            BTreeSet::from(["cython".to_string(), "python".to_string()])
        );
    }

    #[test]
    fn ci_yaml_and_test_json_do_not_invalidate_unrelated_partitions() {
        let base = influence_entries(&[
            (".github/workflows/ci.yml", "base-ci"),
            ("tests/fixtures/snapshot.json", "base-snapshot"),
        ]);
        let target = influence_entries(&[
            (".github/workflows/ci.yml", "target-ci"),
            ("tests/fixtures/snapshot.json", "target-snapshot"),
        ]);

        assert!(changed_partition_signatures(&base, &target).is_empty());
    }

    #[test]
    fn true_shared_config_invalidates_all_partitions() {
        let base = influence_entries(&[(".oh/config.toml", "base-config")]);
        let target = influence_entries(&[(".oh/config.toml", "target-config")]);
        let base_partitions = partition_identities(&base).unwrap();
        let target_partitions = partition_identities(&target).unwrap();
        let shared_changed = influence_digest(&base, SHARED_INFLUENCE_PATTERNS)
            != influence_digest(&target, SHARED_INFLUENCE_PATTERNS);
        let invalidated = base_partitions
            .iter()
            .filter_map(|(language, partition)| {
                (shared_changed || target_partitions[language].signature != partition.signature)
                    .then_some(language.clone())
            })
            .collect::<BTreeSet<_>>();

        assert!(shared_changed);
        assert_eq!(invalidated, base_partitions.into_keys().collect());
    }

    #[test]
    fn execution_digest_detects_copied_or_modified_plan() {
        let mut execution = StructuralCacheExecution {
            schema_version: STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION,
            offline_preprocessing: true,
            base_archive_sha256: "a".repeat(64),
            base_sidecar_sha256: "b".repeat(64),
            base_report_digest: "report".to_string(),
            target_commit: "c".repeat(40),
            target_tree: "d".repeat(40),
            inherited_paths: vec!["src/a.py".to_string()],
            executed_paths: Vec::new(),
            invalidated_partitions: Vec::new(),
            escalated_partitions: Vec::new(),
            changed_file_count: 0,
            invalidated_file_count: 0,
            inherited_graph_enrichment_operation_count: 1,
            executed_graph_enrichment_operation_count: 0,
            inherited_readiness_validation_request_count: 1,
            executed_readiness_validation_request_count: 0,
            executed_producer_work_ids: Vec::new(),
            closure_edge_count: 0,
            execution_job_id: None,
            digest: String::new(),
        };
        execution.finalize();
        assert!(execution.verify_digest());
        execution.executed_paths.push("src/b.py".to_string());
        assert!(!execution.verify_digest());
    }

    #[test]
    fn execution_related_jobs_include_enrichment_and_pass1_producers() {
        let execution = StructuralCacheExecution {
            schema_version: STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION,
            offline_preprocessing: true,
            base_archive_sha256: "a".repeat(64),
            base_sidecar_sha256: "b".repeat(64),
            base_report_digest: "report".to_string(),
            target_commit: "c".repeat(40),
            target_tree: "d".repeat(40),
            inherited_paths: Vec::new(),
            executed_paths: vec!["src/a.py".to_string()],
            invalidated_partitions: vec!["python".to_string()],
            escalated_partitions: Vec::new(),
            changed_file_count: 1,
            invalidated_file_count: 0,
            inherited_graph_enrichment_operation_count: 0,
            executed_graph_enrichment_operation_count: 2,
            inherited_readiness_validation_request_count: 0,
            executed_readiness_validation_request_count: 1,
            executed_producer_work_ids: vec![
                "lsp-pass1-b:1".to_string(),
                "lsp-pass1-a:0".to_string(),
                "lsp-pass1-a:2".to_string(),
            ],
            closure_edge_count: 0,
            execution_job_id: Some("call_references-target".to_string()),
            digest: String::new(),
        };

        assert_eq!(
            execution_related_job_ids(&execution).unwrap(),
            vec![
                "call_references-target".to_string(),
                "lsp-pass1-a".to_string(),
                "lsp-pass1-b".to_string(),
            ]
        );
    }

    #[test]
    fn inherited_readiness_counts_are_exact_for_mixed_language_partitions() {
        use crate::lsp_completeness::{
            FileCoverageRecord, FileRole, FileTerminalStatus, PersistedResults,
        };

        let mut authorization = verified_authorization(
            &[
                ("src/a.py", "python"),
                ("src/b.py", "python"),
                ("src/lib.rs", "rust"),
            ],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let file = |path: &str, language: &str| FileCoverageRecord {
            path: path.to_string(),
            role: FileRole::Source,
            language: Some(language.to_string()),
            expected_server: None,
            advertised_capabilities: Vec::new(),
            requests_attempted: Vec::new(),
            expected_results: BTreeSet::new(),
            expected_result_ids: BTreeSet::new(),
            persisted_results: PersistedResults::default(),
            terminal_status: FileTerminalStatus::Processed { result_count: 0 },
            exclusion: None,
        };
        authorization.base_report.files = vec![
            file("src/a.py", "python"),
            file("src/b.py", "python"),
            file("src/lib.rs", "rust"),
        ];
        authorization
            .base_report
            .readiness_validation_requests_by_language =
            BTreeMap::from([("python".to_string(), 3), ("rust".to_string(), 1)]);

        assert_eq!(
            authorization
                .inherited_readiness_validation_request_count(&BTreeSet::from([
                    PathBuf::from("src/a.py"),
                    PathBuf::from("src/lib.rs"),
                ]))
                .unwrap(),
            2,
            "a partial Python partition inherits one file probe but not its workspace probe"
        );
        assert_eq!(
            authorization
                .inherited_readiness_validation_request_count(&BTreeSet::from([
                    PathBuf::from("src/a.py"),
                    PathBuf::from("src/b.py"),
                    PathBuf::from("src/lib.rs"),
                ]))
                .unwrap(),
            4,
            "a wholly inherited partition also inherits its one non-file probe"
        );
    }

    #[test]
    fn identical_target_reuses_every_authorized_structural_file() {
        let nodes = [
            node("src/a.py", "python"),
            node("src/b.py", "python"),
            node("src/c.rs", "rust"),
        ];
        let authorization = verified_authorization(
            &[
                ("src/a.py", "python"),
                ("src/b.py", "python"),
                ("src/c.rs", "rust"),
            ],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        let plan = plan_incremental_impact(&authorization, &nodes, &[], &nodes, &[]);

        assert!(plan.executed_paths.is_empty());
        assert_eq!(
            plan.inherited_paths,
            ["src/a.py", "src/b.py", "src/c.rs"]
                .into_iter()
                .map(PathBuf::from)
                .collect()
        );
    }

    #[test]
    fn signed_non_inherited_path_is_executed_without_broad_escalation() {
        let nodes = [
            node("src/inherited.py", "python"),
            node("src/no-base-evidence.py", "python"),
        ];
        let mut authorization = verified_authorization(
            &[("src/inherited.py", "python")],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        authorization
            .authorization
            .path_partitions
            .insert("src/no-base-evidence.py".to_string(), "python".to_string());
        authorization
            .authorization
            .executed_operation_budget
            .executed_estimate = 1;

        let plan = plan_incremental_impact(&authorization, &nodes, &[], &nodes, &[]);

        assert_eq!(
            plan.executed_paths,
            BTreeSet::from([PathBuf::from("src/no-base-evidence.py")])
        );
        assert_eq!(
            plan.inherited_paths,
            BTreeSet::from([PathBuf::from("src/inherited.py")])
        );
        assert!(plan.escalated_partitions.is_empty());
        validate_runtime_plan_handoff(&authorization, &plan).unwrap();
    }

    #[test]
    fn changed_python_file_reprocesses_transitive_cross_file_closure() {
        let nodes = [
            node("src/a.py", "python"),
            node("src/b.py", "python"),
            node("src/c.py", "python"),
            node("src/d.py", "python"),
            node("src/e.py", "python"),
            node("src/f.py", "python"),
            node("src/g.py", "python"),
            node("src/h.py", "python"),
        ];
        let edges = [edge(&nodes[0], &nodes[1]), edge(&nodes[1], &nodes[2])];
        let authorization = verified_authorization(
            &[
                ("src/b.py", "python"),
                ("src/c.py", "python"),
                ("src/d.py", "python"),
                ("src/e.py", "python"),
                ("src/f.py", "python"),
                ("src/g.py", "python"),
                ("src/h.py", "python"),
            ],
            &["src/a.py"],
            &[],
            &[],
            &[],
            &[],
            &["src/b.py", "src/c.py"],
        );

        let plan = plan_incremental_impact(&authorization, &nodes, &edges, &nodes, &edges);

        assert_eq!(
            plan.executed_paths,
            ["src/a.py", "src/b.py", "src/c.py"]
                .into_iter()
                .map(PathBuf::from)
                .collect()
        );
        assert!(plan.inherited_paths.contains(Path::new("src/d.py")));
        assert!(!plan.escalated_partitions.contains("python"));
        assert_eq!(plan.closure_edge_count, 2);
        validate_runtime_plan_handoff(&authorization, &plan).unwrap();
    }

    #[test]
    fn reverse_then_forward_edges_reach_the_signed_fixed_point() {
        let nodes = [
            node("src/a.py", "python"),
            node("src/b.py", "python"),
            node("src/c.py", "python"),
            node("src/d.py", "python"),
            node("src/e.py", "python"),
            node("src/f.py", "python"),
            node("src/g.py", "python"),
            node("src/h.py", "python"),
        ];
        let edges = [edge(&nodes[1], &nodes[0]), edge(&nodes[1], &nodes[2])];
        let authorization = verified_authorization(
            &[
                ("src/b.py", "python"),
                ("src/c.py", "python"),
                ("src/d.py", "python"),
                ("src/e.py", "python"),
                ("src/f.py", "python"),
                ("src/g.py", "python"),
                ("src/h.py", "python"),
            ],
            &["src/a.py"],
            &[],
            &[],
            &[],
            &[],
            &["src/b.py", "src/c.py"],
        );

        let plan = plan_incremental_impact(&authorization, &nodes, &edges, &nodes, &edges);

        assert_eq!(
            plan.executed_paths,
            ["src/a.py", "src/b.py", "src/c.py"]
                .into_iter()
                .map(PathBuf::from)
                .collect()
        );
        assert_eq!(plan.closure_edge_count, 2);
        validate_runtime_plan_handoff(&authorization, &plan).unwrap();
    }

    #[test]
    fn runtime_plan_handoff_rejects_one_hop_under_authorized_fixed_point() {
        let nodes = [
            node("src/a.py", "python"),
            node("src/b.py", "python"),
            node("src/c.py", "python"),
            node("src/d.py", "python"),
            node("src/e.py", "python"),
            node("src/f.py", "python"),
            node("src/g.py", "python"),
            node("src/h.py", "python"),
        ];
        let edges = [edge(&nodes[0], &nodes[1]), edge(&nodes[1], &nodes[2])];
        let authorization = verified_authorization(
            &[
                ("src/b.py", "python"),
                ("src/c.py", "python"),
                ("src/d.py", "python"),
                ("src/e.py", "python"),
                ("src/f.py", "python"),
                ("src/g.py", "python"),
                ("src/h.py", "python"),
            ],
            &["src/a.py"],
            &[],
            &[],
            &[],
            &[],
            &["src/b.py"],
        );

        let plan = plan_incremental_impact(&authorization, &nodes, &edges, &nodes, &edges);
        let error = validate_runtime_plan_handoff(&authorization, &plan).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("expected=2 actual=3 first_unexpected=src/c.py first_missing=-")
        );
    }

    #[test]
    fn verified_changed_file_scheduler_accepts_node_dense_authorized_plan() {
        let mut authorization =
            verified_authorization(&[], &["src/dense.py"], &[], &[], &[], &[], &[]);
        authorization
            .authorization
            .executed_operation_budget
            .executed_estimate = 7_523;
        let plan = IncrementalImpactPlan {
            executed_paths: BTreeSet::from([PathBuf::from("src/dense.py")]),
            inherited_paths: BTreeSet::new(),
            escalated_partitions: BTreeSet::new(),
            closure_edge_count: 0,
        };
        let nodes = (0..6_067)
            .map(|index| {
                let mut node = node("src/dense.py", "python");
                node.id.name = format!("symbol_{index}");
                node
            })
            .collect::<Vec<_>>();

        let ids = crate::server::plan_lsp_node_ids_for_verified_structural_cache(
            &authorization,
            &plan,
            &nodes,
        )
        .unwrap();
        assert_eq!(ids.len(), 6_067);
    }

    #[test]
    fn verified_changed_file_scheduler_still_enforces_operation_ceiling() {
        let mut authorization =
            verified_authorization(&[], &["src/operation-heavy.py"], &[], &[], &[], &[], &[]);
        authorization
            .authorization
            .executed_operation_budget
            .executed_estimate = 1;
        let plan = IncrementalImpactPlan {
            executed_paths: BTreeSet::from([PathBuf::from("src/operation-heavy.py")]),
            inherited_paths: BTreeSet::new(),
            escalated_partitions: BTreeSet::new(),
            closure_edge_count: 0,
        };
        let nodes = (0..=crate::extract::lsp::MAX_INCREMENTAL_LSP_OPERATIONS)
            .map(|index| {
                let mut node = node("src/operation-heavy.py", "python");
                node.id.name = format!("symbol_{index}");
                node
            })
            .collect::<Vec<_>>();

        let error = crate::server::plan_lsp_node_ids_for_verified_structural_cache(
            &authorization,
            &plan,
            &nodes,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("changed-file LSP plan exceeds its bound")
        );
    }

    #[test]
    fn carried_lsp_nodes_do_not_escalate_verified_impact_plan() {
        let changed = node("src/changed.py", "python");
        let unchanged = node("src/unchanged.py", "python");
        let authorization = verified_authorization(
            &[("src/unchanged.py", "python")],
            &["src/changed.py"],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let mut new_nodes = vec![changed.clone(), unchanged.clone()];
        new_nodes.extend((0..=MAX_INCREMENTAL_LSP_OPERATIONS).map(|index| {
            let mut output = node("src/changed.py", "python");
            output.id.name = format!("persisted_lsp_output_{index}");
            output.source = ExtractionSource::Lsp;
            output
        }));

        let plan =
            plan_incremental_impact(&authorization, &[changed, unchanged], &[], &new_nodes, &[]);

        assert_eq!(
            plan.executed_paths,
            BTreeSet::from([PathBuf::from("src/changed.py")])
        );
        assert_eq!(
            plan.inherited_paths,
            BTreeSet::from([PathBuf::from("src/unchanged.py")])
        );
        assert!(plan.escalated_partitions.is_empty());
        validate_runtime_plan_handoff(&authorization, &plan).unwrap();
    }

    #[test]
    fn verified_changed_file_scheduler_excludes_carried_lsp_nodes() {
        let authorization =
            verified_authorization(&[], &["src/changed.py"], &[], &[], &[], &[], &[]);
        let plan = IncrementalImpactPlan {
            executed_paths: BTreeSet::from([PathBuf::from("src/changed.py")]),
            inherited_paths: BTreeSet::new(),
            escalated_partitions: BTreeSet::new(),
            closure_edge_count: 0,
        };
        let seed = node("src/changed.py", "python");
        let seed_id = seed.stable_id();
        let mut nodes = vec![seed];
        nodes.extend(
            (0..=crate::extract::lsp::MAX_INCREMENTAL_LSP_OPERATIONS).map(|index| {
                let mut output = node("src/changed.py", "python");
                output.id.name = format!("persisted_lsp_output_{index}");
                output.source = ExtractionSource::Lsp;
                output
            }),
        );

        let ids = crate::server::plan_lsp_node_ids_for_verified_structural_cache(
            &authorization,
            &plan,
            &nodes,
        )
        .unwrap();

        assert_eq!(ids.as_ref(), &std::collections::HashSet::from([seed_id]));
    }

    #[test]
    fn verified_changed_file_scheduler_rejects_handoff_mismatch() {
        let authorization =
            verified_authorization(&[], &["src/authorized.py"], &[], &[], &[], &[], &[]);
        let plan = IncrementalImpactPlan {
            executed_paths: BTreeSet::from([PathBuf::from("src/unexpected.py")]),
            inherited_paths: BTreeSet::new(),
            escalated_partitions: BTreeSet::new(),
            closure_edge_count: 0,
        };

        let error = crate::server::plan_lsp_node_ids_for_verified_structural_cache(
            &authorization,
            &plan,
            &[],
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("runtime structural-cache execution differs from verifier authorization")
        );
    }

    #[test]
    fn only_carried_lsp_edges_expand_the_fixed_point() {
        let nodes = [
            node("src/a.py", "python"),
            node("src/b.py", "python"),
            node("src/c.py", "python"),
            node("src/d.py", "python"),
            node("src/e.py", "python"),
            node("src/f.py", "python"),
        ];
        let direct_lsp_edge = edge(&nodes[0], &nodes[1]);
        let second_hop_lsp_edge = edge(&nodes[1], &nodes[2]);
        let mut old_static_edge = edge(&nodes[0], &nodes[3]);
        old_static_edge.source = ExtractionSource::TreeSitter;
        let new_lsp_edge = edge(&nodes[0], &nodes[4]);
        let authorization = verified_authorization(
            &[
                ("src/b.py", "python"),
                ("src/c.py", "python"),
                ("src/d.py", "python"),
                ("src/e.py", "python"),
                ("src/f.py", "python"),
            ],
            &["src/a.py"],
            &[],
            &[],
            &[],
            &[],
            &["src/b.py", "src/c.py"],
        );

        let plan = plan_incremental_impact(
            &authorization,
            &nodes,
            &[direct_lsp_edge, second_hop_lsp_edge, old_static_edge],
            &nodes,
            &[new_lsp_edge],
        );

        assert_eq!(
            plan.executed_paths,
            ["src/a.py", "src/b.py", "src/c.py"]
                .into_iter()
                .map(PathBuf::from)
                .collect()
        );
        assert!(plan.inherited_paths.contains(Path::new("src/d.py")));
        assert!(plan.inherited_paths.contains(Path::new("src/e.py")));
        assert_eq!(plan.closure_edge_count, 2);
        validate_runtime_plan_handoff(&authorization, &plan).unwrap();
    }

    #[test]
    fn materialized_lsp_endpoint_with_inventory_path_is_in_fixed_point() {
        let changed = node("src/a.py", "python");
        let mut materialized_endpoint = node("src/b.py", "python");
        materialized_endpoint.id.name = "synthetic::target".to_string();
        materialized_endpoint.source = ExtractionSource::Lsp;
        let downstream = node("src/c.py", "python");
        let remaining = [
            node("src/d.py", "python"),
            node("src/e.py", "python"),
            node("src/f.py", "python"),
            node("src/g.py", "python"),
            node("src/h.py", "python"),
        ];
        let mut nodes = vec![changed, materialized_endpoint, downstream];
        nodes.extend(remaining);
        let edges = [edge(&nodes[0], &nodes[1]), edge(&nodes[1], &nodes[2])];
        let authorization = verified_authorization(
            &[
                ("src/b.py", "python"),
                ("src/c.py", "python"),
                ("src/d.py", "python"),
                ("src/e.py", "python"),
                ("src/f.py", "python"),
                ("src/g.py", "python"),
                ("src/h.py", "python"),
            ],
            &["src/a.py"],
            &[],
            &[],
            &[],
            &[],
            &["src/b.py", "src/c.py"],
        );

        let plan = plan_incremental_impact(&authorization, &nodes, &edges, &nodes, &edges);

        assert_eq!(
            plan.executed_paths,
            ["src/a.py", "src/b.py", "src/c.py"]
                .into_iter()
                .map(PathBuf::from)
                .collect()
        );
        assert_eq!(plan.closure_edge_count, 2);
        validate_runtime_plan_handoff(&authorization, &plan).unwrap();
    }

    #[test]
    fn partition_escalation_recloses_crossing_lsp_edges_until_stable() {
        let nodes = [
            node("src/a.py", "python"),
            node("src/b.py", "python"),
            node("src/c.py", "python"),
            node("src/d.py", "python"),
            node("src/e.py", "python"),
            node("src/f.py", "python"),
            node("src/r.rs", "rust"),
            node("src/s.rs", "rust"),
        ];
        let edges = [
            edge(&nodes[0], &nodes[1]),
            edge(&nodes[1], &nodes[2]),
            edge(&nodes[2], &nodes[3]),
            edge(&nodes[3], &nodes[4]),
            edge(&nodes[5], &nodes[6]),
        ];
        let authorization = verified_authorization(
            &[
                ("src/b.py", "python"),
                ("src/c.py", "python"),
                ("src/d.py", "python"),
                ("src/e.py", "python"),
                ("src/f.py", "python"),
                ("src/r.rs", "rust"),
                ("src/s.rs", "rust"),
            ],
            &["src/a.py"],
            &[],
            &[],
            &[],
            &[],
            &["src/b.py", "src/c.py", "src/d.py", "src/e.py"],
        );

        let plan = plan_incremental_impact(&authorization, &nodes, &edges, &nodes, &edges);

        assert_eq!(
            plan.executed_paths,
            [
                "src/a.py", "src/b.py", "src/c.py", "src/d.py", "src/e.py", "src/f.py", "src/r.rs",
                "src/s.rs",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect()
        );
        assert_eq!(
            plan.escalated_partitions,
            BTreeSet::from(["python".to_string(), "rust".to_string()])
        );
        assert_eq!(plan.closure_edge_count, 5);
        let error = validate_runtime_plan_handoff(&authorization, &plan).unwrap_err();
        assert!(error.to_string().contains("first_unexpected=src/f.py"));
    }

    #[test]
    fn retained_typed_output_node_cannot_touch_signed_execution_path() {
        let mut output = node("src/executed.py", "python");
        output.source = ExtractionSource::Lsp;
        let record = crate::extract::lsp::work_items::LspWorkItemRecord {
            file: "src/inherited.py".to_string(),
            output_nodes: vec![output],
            ..Default::default()
        };

        let detail = retained_output_touching_executed_path(
            &record,
            "fixture",
            &BTreeSet::from([PathBuf::from("src/executed.py")]),
        )
        .expect("crossing typed node must fail closed");

        assert!(detail.contains("typed output node"));
        assert!(detail.contains("src/executed.py"));
    }

    #[test]
    fn retained_typed_output_edges_cannot_cross_in_either_orientation() {
        let inherited = node("src/inherited.py", "python");
        let executed = node("src/executed.py", "python");
        let executed_paths = BTreeSet::from([PathBuf::from("src/executed.py")]);
        for crossing in [edge(&inherited, &executed), edge(&executed, &inherited)] {
            let record = crate::extract::lsp::work_items::LspWorkItemRecord {
                file: "src/inherited.py".to_string(),
                output_edges: vec![crossing],
                ..Default::default()
            };
            let detail =
                retained_output_touching_executed_path(&record, "fixture", &executed_paths)
                    .expect("crossing typed edge must fail closed");
            assert!(detail.contains("typed output edge"));
            assert!(detail.contains("src/executed.py"));
        }
    }

    #[test]
    fn non_inventory_external_hub_cannot_expand_direct_impact() {
        let changed = node("src/a.py", "python");
        let inherited = node("src/b.py", "python");
        let mut external_hub = node("builtins.py", "python");
        external_hub.id.root = "external".to_string();
        external_hub.source = ExtractionSource::Lsp;
        let nodes = [changed, inherited, external_hub];
        let edges = [edge(&nodes[0], &nodes[2]), edge(&nodes[2], &nodes[1])];
        let authorization = verified_authorization(
            &[("src/b.py", "python")],
            &["src/a.py"],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        let plan = plan_incremental_impact(&authorization, &nodes, &edges, &nodes, &edges);

        assert_eq!(
            plan.executed_paths,
            BTreeSet::from([PathBuf::from("src/a.py")])
        );
        assert!(plan.inherited_paths.contains(Path::new("src/b.py")));
        assert!(!plan.executed_paths.contains(Path::new("builtins.py")));
        assert_eq!(plan.closure_edge_count, 0);
    }

    #[test]
    fn pathless_external_lsp_endpoints_never_become_execution_paths() {
        let local = node("src/parser.pyx", "cython");
        let helper = node("src/helper.pyx", "cython");
        let mut external_cython = node("", "cython");
        external_cython.id.root = "external".to_string();
        external_cython.source = ExtractionSource::Lsp;
        let mut external_unknown = node("", "");
        external_unknown.id.root = "external".to_string();
        external_unknown.source = ExtractionSource::Lsp;
        let nodes = [local, helper, external_cython, external_unknown];
        let edges = [edge(&nodes[0], &nodes[2]), edge(&nodes[1], &nodes[3])];
        let authorization = verified_authorization(
            &[("src/helper.pyx", "cython")],
            &[],
            &["src/parser.pyx"],
            &[],
            &[],
            &["cython"],
            &[],
        );

        let plan = plan_incremental_impact(&authorization, &nodes, &edges, &nodes, &edges);

        assert_eq!(
            plan.executed_paths,
            ["src/helper.pyx", "src/parser.pyx"]
                .into_iter()
                .map(PathBuf::from)
                .collect()
        );
        assert!(plan.escalated_partitions.contains("cython"));
        assert!(!plan.escalated_partitions.contains(""));
        for path in &plan.executed_paths {
            let path = path.to_str().expect("execution path must be UTF-8");
            assert_eq!(
                crate::lsp_completeness::normalize_repo_relative_path(path).as_deref(),
                Ok(path)
            );
        }
    }

    #[test]
    fn symbol_dense_above_node_ceiling_below_operation_ceiling_remains_incremental() {
        let nodes = (0..=4_096)
            .map(|index| {
                let mut dense = node("src/dense.py", "python");
                dense.id.name = format!("symbol_{index}");
                dense.signature = format!("def symbol_{index}():");
                dense
            })
            .collect::<Vec<_>>();
        let authorization = verified_authorization(
            &[("src/unchanged.py", "python")],
            &["src/dense.py"],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        let plan = plan_incremental_impact(&authorization, &nodes, &[], &nodes, &[]);

        assert_eq!(
            plan.executed_paths,
            BTreeSet::from([PathBuf::from("src/dense.py")])
        );
        assert_eq!(
            plan.inherited_paths,
            BTreeSet::from([PathBuf::from("src/unchanged.py")])
        );
        assert!(plan.escalated_partitions.is_empty());
        validate_runtime_plan_handoff(&authorization, &plan).unwrap();
    }

    #[test]
    fn test_file_functions_above_operation_ceiling_do_not_escalate_or_schedule() {
        let nodes = (0..=MAX_INCREMENTAL_LSP_OPERATIONS)
            .map(|index| {
                let mut test = node("tests/test_dense.py", "python");
                test.id.name = format!("test_symbol_{index}");
                test.signature = format!("def test_symbol_{index}():");
                test
            })
            .collect::<Vec<_>>();
        let authorization = verified_authorization(
            &[("src/unchanged.py", "python")],
            &["tests/test_dense.py"],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        let plan = plan_incremental_impact(&authorization, &nodes, &[], &nodes, &[]);

        assert_eq!(
            plan.executed_paths,
            BTreeSet::from([PathBuf::from("tests/test_dense.py")])
        );
        assert_eq!(
            plan.inherited_paths,
            BTreeSet::from([PathBuf::from("src/unchanged.py")])
        );
        assert!(plan.escalated_partitions.is_empty());
        validate_runtime_plan_handoff(&authorization, &plan).unwrap();
        let scheduled = crate::server::plan_lsp_node_ids_for_verified_structural_cache(
            &authorization,
            &plan,
            &nodes,
        )
        .unwrap();
        assert!(scheduled.is_empty());
    }

    #[test]
    fn operation_over_bound_update_escalates_descriptor_partition() {
        let mut nodes = (0..=MAX_INCREMENTAL_LSP_OPERATIONS)
            .map(|index| {
                let mut dense = node("src/dense.py", "python");
                dense.id.name = format!("symbol_{index}");
                dense.signature = format!("def symbol_{index}():");
                dense
            })
            .collect::<Vec<_>>();
        nodes.push(node("src/unchanged.py", "python"));
        let mut authorization = verified_authorization(
            &[("src/unchanged.py", "python")],
            &["src/dense.py"],
            &[],
            &[],
            &[],
            &[],
            &[],
        );
        authorization
            .authorization
            .executed_operation_budget
            .executed_estimate = MAX_INCREMENTAL_LSP_OPERATIONS as u64 + 1;

        let plan = plan_incremental_impact(&authorization, &nodes, &[], &nodes, &[]);

        assert_eq!(
            plan.executed_paths,
            ["src/dense.py", "src/unchanged.py"]
                .into_iter()
                .map(PathBuf::from)
                .collect()
        );
        assert!(plan.inherited_paths.is_empty());
        assert_eq!(
            plan.escalated_partitions,
            BTreeSet::from(["python".to_string()])
        );
        let error = validate_runtime_plan_handoff(&authorization, &plan).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("first_unexpected=src/unchanged.py")
        );
    }

    #[test]
    fn changed_declaration_surface_uses_bounded_graph_closure() {
        let old_nodes = [
            node("src/a.py", "python"),
            node("src/b.py", "python"),
            node("src/c.py", "python"),
        ];
        let mut new_nodes = old_nodes.clone();
        new_nodes[0].signature = "def a(new_public_parameter):".to_string();
        let authorization = verified_authorization(
            &[("src/b.py", "python"), ("src/c.py", "python")],
            &["src/a.py"],
            &[],
            &[],
            &[],
            &[],
            &[],
        );

        let plan = plan_incremental_impact(&authorization, &old_nodes, &[], &new_nodes, &[]);

        assert!(!plan.escalated_partitions.contains("python"));
        assert_eq!(
            plan.executed_paths,
            BTreeSet::from([PathBuf::from("src/a.py")])
        );
        assert_eq!(
            plan.inherited_paths,
            ["src/b.py", "src/c.py"]
                .into_iter()
                .map(PathBuf::from)
                .collect()
        );
    }

    #[test]
    fn added_document_does_not_rebuild_changed_python_partition() {
        let old_nodes = [
            node("src/a.py", "python"),
            node("src/b.py", "python"),
            node("src/c.py", "python"),
            node("src/d.py", "python"),
            node("src/e.py", "python"),
            node("src/f.py", "python"),
        ];
        let mut new_nodes = old_nodes.to_vec();
        new_nodes[0].signature = "def a(changed):".to_string();
        new_nodes.push(node("docs/release.txt", "plaintext"));
        let edges = [edge(&old_nodes[0], &old_nodes[2])];
        let authorization = verified_authorization(
            &[
                ("src/b.py", "python"),
                ("src/c.py", "python"),
                ("src/d.py", "python"),
                ("src/e.py", "python"),
                ("src/f.py", "python"),
            ],
            &["src/a.py"],
            &["docs/release.txt"],
            &[],
            &[],
            &[],
            &[],
        );

        let plan = plan_incremental_impact(&authorization, &old_nodes, &edges, &new_nodes, &edges);

        assert_eq!(
            plan.executed_paths,
            ["docs/release.txt", "src/a.py", "src/c.py"]
                .into_iter()
                .map(PathBuf::from)
                .collect()
        );
        assert!(plan.inherited_paths.contains(Path::new("src/b.py")));
        assert!(!plan.escalated_partitions.contains("python"));
    }

    #[test]
    fn delete_rename_and_inventory_only_partition_paths_are_invalidated() {
        let old_nodes = [
            node("src/old.py", "python"),
            node("src/rename_old.py", "python"),
            node("src/keep.rs", "rust"),
        ];
        let new_nodes = [
            node("src/rename_new.py", "python"),
            node("src/keep.rs", "rust"),
        ];
        let authorization = verified_authorization(
            &[("src/keep.rs", "rust")],
            &[],
            &[],
            &["src/old.py"],
            &[["src/rename_old.py", "src/rename_new.py"]],
            &["python"],
            &["docs/zero_symbol.py"],
        );

        let plan = plan_incremental_impact(&authorization, &old_nodes, &[], &new_nodes, &[]);

        for path in [
            "src/old.py",
            "src/rename_old.py",
            "src/rename_new.py",
            "docs/zero_symbol.py",
        ] {
            assert!(
                plan.executed_paths.contains(Path::new(path)),
                "missing {path}"
            );
        }
        assert!(plan.inherited_paths.contains(Path::new("src/keep.rs")));
        assert!(!plan.executed_paths.contains(Path::new("src/keep.rs")));
    }

    #[test]
    fn authorization_and_full_record_digests_detect_tampering() {
        let mut verified =
            verified_authorization(&[("src/a.py", "python")], &[], &[], &[], &[], &[], &[]);
        verified.authorization.finalize().unwrap();
        assert!(verified.authorization.verify_digest());
        verified
            .authorization
            .inherited_files
            .first_mut()
            .unwrap()
            .blob = "0".repeat(40);
        assert!(!verified.authorization.verify_digest());

        let original = node("src/a.py", "python");
        let mut tampered = original.clone();
        tampered.body = "changed without changing stable id".to_string();
        assert_ne!(
            graph_records_digest(&[original], Node::stable_id).unwrap(),
            graph_records_digest(&[tampered], Node::stable_id).unwrap()
        );
    }

    #[test]
    fn full_record_digest_is_order_independent_for_duplicate_stable_ids() {
        let first = node("src/a.py", "python");
        let mut second = first.clone();
        second.body = "same identity, distinct full record".to_string();
        assert_eq!(
            graph_records_digest(&[first.clone(), second.clone()], Node::stable_id).unwrap(),
            graph_records_digest(&[second, first], Node::stable_id).unwrap()
        );
    }
}
