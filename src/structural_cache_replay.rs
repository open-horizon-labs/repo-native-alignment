//! Hidden diagnostic replay for a retained post-LSP/pre-readiness failure cache.
//!
//! This path is intentionally non-publishable. It never scans a checkout,
//! starts a language server, extracts an archive, or catalogs an output cache.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::business_context::BusinessContextMode;
use crate::extract::lsp::work_items::{LspWorkItemRecord, LspWorkItemState};
use crate::extract::scan_stats::{
    LSP_VALIDATION_EVIDENCE_SCHEMA_VERSION, LspNegotiatedCapabilities, LspValidationEvidence,
    LspValidationStatus,
};
use crate::graph::{Edge, Node};
use crate::server::{
    EnrichmentCapability, EnrichmentJobLedger, EnrichmentJobRecord, EnrichmentJobState,
    EnrichmentScope, EnrichmentTrigger,
};
use crate::structural_cache::{
    IncrementalImpactPlan, QUALIFICATION_SCAN_FLAGS, STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION,
    StructuralCacheAuthorization, StructuralProducerIdentity, VerifiedStructuralCacheAuthorization,
};

#[derive(Debug)]
pub struct StructuralCacheReplayRequest<'a> {
    pub repo_root: &'a Path,
    pub failure_receipt: &'a Path,
    pub failure_receipt_sha256: &'a str,
    pub authorization_sha256: &'a str,
    pub toolchain_lock_digest: &'a str,
    pub inventory_digest: &'a str,
    pub inventory_file_sha256: &'a str,
    pub configuration_digest: &'a str,
    pub repository: &'a str,
    pub root_slug: &'a str,
    pub target_commit: &'a str,
    pub target_tree: &'a str,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StructuralCacheReplayReceipt {
    pub schema_version: u32,
    pub diagnostic_only: bool,
    pub publishable: bool,
    pub checkout_rebuilt: bool,
    pub lsp_calls: u64,
    pub archive_created: bool,
    pub catalog_updated: bool,
    pub failure_receipt_sha256: String,
    pub failure_digest: String,
    pub authorization_sha256: String,
    pub source_producer_commit: String,
    pub replay_producer_commit: String,
    pub source_producer: StructuralProducerIdentity,
    pub replay_producer: StructuralProducerIdentity,
    pub target_commit: String,
    pub target_tree: String,
    pub target_tree_source: String,
    pub source_checkout_identity_verified: bool,
    pub source_tree_diff_replayed: bool,
    pub source_rescanned: bool,
    pub full_target_readiness_recomputed: bool,
    pub incremental_enrichment_job_id: String,
    pub pass1_job_ids: Vec<String>,
    pub initial_node_count: u64,
    pub initial_edge_count: u64,
    pub stale_path_count: u64,
    pub stale_node_count_before: u64,
    pub stale_edge_count_before: u64,
    pub removed_node_count: u64,
    pub removed_edge_count: u64,
    pub final_node_count: u64,
    pub final_edge_count: u64,
    pub completed_work_item_count: u64,
    pub executed_operation_count: u64,
    pub readiness_validation_request_count: u64,
    pub base_completeness_digest: String,
    pub target_inventory_path_count: u64,
    pub validated_inventory_path_count: u64,
    pub observed_result_count: u64,
    pub persisted_observed_result_count: u64,
    pub persisted_result_id_count: u64,
    pub unresolved_endpoint_count: u64,
    pub discarded_required_result_count: u64,
    pub checkpoint_validation_digest: String,
    pub diagnostic_checkpoint_validation_passed: bool,
    pub target_completeness_digest: String,
    pub coverage_violation_count: u64,
    pub compatibility_violation_count: u64,
    pub full_target_ready: bool,
}

#[derive(Debug, Serialize)]
struct CheckpointValidationSummary {
    target_inventory_path_count: u64,
    validated_inventory_path_count: u64,
    observed_result_count: u64,
    persisted_observed_result_count: u64,
    persisted_result_id_count: u64,
    unresolved_endpoint_count: u64,
    discarded_required_result_count: u64,
}

#[derive(Debug, Deserialize)]
struct RetainedFailureReceipt {
    status: String,
    failure_digest: String,
    evidence: Vec<RetainedEvidenceFile>,
}

#[derive(Debug, Deserialize)]
struct RetainedEvidenceFile {
    cache_path: String,
    sha256: String,
    size_bytes: u64,
}

pub async fn replay_retained_structural_cache(
    request: &StructuralCacheReplayRequest<'_>,
) -> Result<StructuralCacheReplayReceipt> {
    for (value, label) in [
        (request.failure_receipt_sha256, "failure receipt SHA-256"),
        (request.authorization_sha256, "authorization SHA-256"),
        (request.toolchain_lock_digest, "toolchain lock digest"),
        (request.inventory_digest, "inventory digest"),
        (request.inventory_file_sha256, "inventory file SHA-256"),
        (request.configuration_digest, "configuration digest"),
    ] {
        require_sha256(value, label)?;
    }
    require_git_oid(request.target_commit, "target commit")?;
    require_git_oid(request.target_tree, "target tree")?;

    let cache_root = request.repo_root.join(".oh/.cache");
    let failure_receipt = verify_retained_evidence(
        &cache_root,
        request.failure_receipt,
        request.failure_receipt_sha256,
    )?;
    ensure!(
        failure_receipt.status == "failed",
        "retained receipt is not a failure"
    );
    require_sha256(&failure_receipt.failure_digest, "failure digest")?;

    let authorization = load_replay_authorization(request)?;
    let source_producer_commit = authorization.authorization.producer.producer_commit.clone();
    let current_identity = crate::structural_cache::current_identity(
        request.repo_root,
        BusinessContextMode::Disabled,
    )?;

    let retained_execution = crate::structural_cache::load_execution(request.repo_root)?;
    if let Some(execution) = &retained_execution {
        ensure!(
            execution.base_archive_sha256 == authorization.authorization.base_archive_sha256
                && execution.base_sidecar_sha256 == authorization.authorization.base_sidecar_sha256
                && execution.base_report_digest == authorization.authorization.base_report_digest
                && execution.target_commit == authorization.authorization.target_commit
                && execution.target_tree == authorization.authorization.target_tree,
            "retained target execution evidence does not match replay authorization"
        );
    }

    let target_job = unique_completed_incremental_job(request.repo_root)?;
    let validations = target_job
        .lsp_evidence
        .as_ref()
        .context("incremental job is missing persisted LSP validation evidence")?
        .validations
        .clone();
    let all_work_items = crate::extract::lsp::work_items::load_all_records(request.repo_root)?;
    validate_replay_work_items(&all_work_items, &target_job)?;

    let mut graph = crate::server::load_graph_from_lance(request.repo_root)
        .await
        .context("load retained post-LSP graph")?;
    let initial_node_count = graph.nodes.len();
    let initial_edge_count = graph.edges.len();
    let stale_paths = stale_paths(&authorization.authorization);
    ensure!(
        !stale_paths.is_empty(),
        "diagnostic replay has no stale paths to purge"
    );
    let (stale_node_count_before, stale_edge_count_before) =
        stale_graph_record_counts(&graph.nodes, &graph.edges, &stale_paths);

    let node_filter = HashSet::new();
    let file_filter = stale_paths.iter().cloned().collect::<HashSet<_>>();
    crate::server::purge_existing_scoped_lsp_output(
        &mut graph.nodes,
        &mut graph.edges,
        &node_filter,
        &file_filter,
    );
    let removed_node_count = initial_node_count.saturating_sub(graph.nodes.len());
    let removed_edge_count = initial_edge_count.saturating_sub(graph.edges.len());

    crate::server::persist_graph_to_lance(request.repo_root, &graph.nodes, &graph.edges)
        .await
        .context("persist replay-purged graph")?;
    crate::structural_cache::validate_persisted_target(
        request.repo_root,
        &graph.nodes,
        &graph.edges,
        &stale_paths,
    )
    .await
    .context("fresh reopen after replay purge")?;

    let reopened = crate::server::load_graph_from_lance(request.repo_root).await?;
    let plan = diagnostic_execution_plan(&authorization, &reopened.nodes, &reopened.edges);
    let checkpoint = validate_retained_checkpoint(
        request.repo_root,
        &authorization,
        &plan,
        &all_work_items,
        &validations,
        &reopened.nodes,
        &reopened.edges,
    )?;
    let checkpoint_validation_digest = sha256_bytes(&serde_json::to_vec(&checkpoint)?);

    let execution = crate::structural_cache::build_execution(
        request.repo_root,
        &authorization,
        &plan,
        &validations,
        Some(target_job.job_id.clone()),
        0,
    )?;
    ensure!(
        execution.executed_producer_work_ids.len() == all_work_items.len(),
        "diagnostic execution evidence omitted retained target work items"
    );
    crate::structural_cache::write_execution(request.repo_root, &execution)?;
    let related_job_ids = crate::structural_cache::execution_related_job_ids(&execution)?;
    let pass1_job_ids = related_job_ids
        .iter()
        .filter(|job_id| *job_id != &target_job.job_id)
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        !pass1_job_ids.is_empty(),
        "diagnostic replay found no pass1 job IDs"
    );

    let report = crate::lsp_completeness::build_and_persist_report_from_evidence(
        request.repo_root,
        BusinessContextMode::Disabled,
        &reopened.nodes,
        &reopened.edges,
        &[],
        &related_job_ids,
        0,
        Some(&authorization),
        Some(&execution),
    )?;
    let final_reopen = crate::server::load_graph_from_lance(request.repo_root).await?;
    let check = crate::lsp_completeness::load_readiness_check_with_graph(
        request.repo_root,
        BusinessContextMode::Disabled,
        &final_reopen.nodes,
        &final_reopen.edges,
    )?;

    Ok(StructuralCacheReplayReceipt {
        schema_version: 1,
        diagnostic_only: true,
        publishable: false,
        checkout_rebuilt: false,
        lsp_calls: 0,
        archive_created: false,
        catalog_updated: false,
        failure_receipt_sha256: request.failure_receipt_sha256.to_string(),
        failure_digest: failure_receipt.failure_digest,
        authorization_sha256: request.authorization_sha256.to_string(),
        source_producer_commit,
        replay_producer_commit: current_identity.producer.producer_commit.clone(),
        source_producer: authorization.authorization.producer.clone(),
        replay_producer: current_identity.producer.clone(),
        target_commit: request.target_commit.to_string(),
        target_tree: request.target_tree.to_string(),
        target_tree_source: "copied_retained_checkout_and_verified_authorization".to_string(),
        source_checkout_identity_verified: true,
        source_tree_diff_replayed: false,
        source_rescanned: false,
        full_target_readiness_recomputed: true,
        incremental_enrichment_job_id: target_job.job_id,
        pass1_job_ids,
        initial_node_count: initial_node_count as u64,
        initial_edge_count: initial_edge_count as u64,
        stale_path_count: stale_paths.len() as u64,
        stale_node_count_before: stale_node_count_before as u64,
        stale_edge_count_before: stale_edge_count_before as u64,
        removed_node_count: removed_node_count as u64,
        removed_edge_count: removed_edge_count as u64,
        final_node_count: final_reopen.nodes.len() as u64,
        final_edge_count: final_reopen.edges.len() as u64,
        completed_work_item_count: all_work_items.len() as u64,
        executed_operation_count: execution.executed_graph_enrichment_operation_count,
        readiness_validation_request_count: execution.executed_readiness_validation_request_count,
        base_completeness_digest: authorization.base_report.digest.clone(),
        target_inventory_path_count: checkpoint.target_inventory_path_count,
        validated_inventory_path_count: checkpoint.validated_inventory_path_count,
        observed_result_count: checkpoint.observed_result_count,
        persisted_observed_result_count: checkpoint.persisted_observed_result_count,
        persisted_result_id_count: checkpoint.persisted_result_id_count,
        unresolved_endpoint_count: checkpoint.unresolved_endpoint_count,
        discarded_required_result_count: checkpoint.discarded_required_result_count,
        checkpoint_validation_digest,
        diagnostic_checkpoint_validation_passed: true,
        target_completeness_digest: report.digest,
        coverage_violation_count: check.report.violations.len() as u64,
        compatibility_violation_count: check.compatibility_violations.len() as u64,
        full_target_ready: check.ready,
    })
}

fn load_replay_authorization(
    request: &StructuralCacheReplayRequest<'_>,
) -> Result<VerifiedStructuralCacheAuthorization> {
    let path = crate::structural_cache::authorization_path(request.repo_root);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("inspect replay authorization {}", path.display()))?;
    ensure!(
        metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
        "replay authorization must be a regular non-symlink file"
    );
    let bytes = fs::read(&path)?;
    ensure!(
        sha256_bytes(&bytes) == request.authorization_sha256,
        "replay authorization SHA-256 mismatch"
    );
    let authorization: StructuralCacheAuthorization =
        serde_json::from_slice(&bytes).context("invalid replay structural-cache authorization")?;
    ensure!(
        authorization.schema_version == STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION,
        "replay authorization schema mismatch"
    );
    ensure!(
        authorization.offline_preprocessing,
        "replay authorization is not offline preprocessing"
    );
    ensure!(
        authorization.verify_digest(),
        "replay authorization digest mismatch"
    );
    ensure!(
        authorization.toolchain_lock_digest == request.toolchain_lock_digest
            && authorization.inventory_digest == request.inventory_digest
            && authorization.inventory_file_sha256 == request.inventory_file_sha256
            && authorization.configuration_digest == request.configuration_digest,
        "replay toolchain/inventory/configuration binding mismatch"
    );
    ensure!(
        authorization.scan_flags
            == QUALIFICATION_SCAN_FLAGS
                .iter()
                .map(|flag| (*flag).to_string())
                .collect::<Vec<_>>(),
        "replay scan flags mismatch"
    );
    ensure!(
        authorization.repository == request.repository
            && authorization.root_slug == request.root_slug,
        "replay repository/root binding mismatch"
    );
    ensure!(
        authorization.target_commit == request.target_commit
            && authorization.target_tree == request.target_tree,
        "replay target commit/tree mismatch"
    );

    let identity = crate::structural_cache::current_identity(
        request.repo_root,
        BusinessContextMode::Disabled,
    )?;
    ensure!(
        identity.repository == authorization.repository
            && identity.commit == authorization.target_commit
            && identity.tree == authorization.target_tree
            && identity.root_slug == authorization.root_slug
            && identity.configuration_digest == authorization.configuration_digest,
        "replay authorization does not match the target checkout"
    );
    let mut schema_only_producer = authorization.producer.clone();
    schema_only_producer.producer_commit = identity.producer.producer_commit.clone();
    schema_only_producer.binary_sha256 = identity.producer.binary_sha256.clone();
    ensure!(
        schema_only_producer == identity.producer,
        "replay permits producer commit/binary drift only; a schema or package identity changed"
    );
    let invalidated = authorization
        .invalidated_partitions
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    ensure!(
        invalidated
            .iter()
            .all(|language| identity.partitions.contains_key(language)),
        "diagnostic replay authorization invalidates an unknown language partition"
    );
    ensure!(
        authorization
            .path_partitions
            .values()
            .all(|language| identity.partitions.contains_key(language)),
        "replay authorization contains an unknown language partition"
    );

    let base_report = crate::lsp_completeness::load_report(request.repo_root)?;
    ensure!(
        base_report.digest == authorization.base_report_digest
            && base_report.identity.checkout_sha == authorization.base_commit
            && base_report.is_ready()
            && base_report.integrity_violations().is_empty(),
        "replay base report is not verifier-clean READY evidence"
    );
    let base_report_bytes = fs::read(crate::lsp_completeness::report_path(request.repo_root))?;
    ensure!(
        sha256_bytes(&base_report_bytes) == authorization.base_report_sha256,
        "replay base report SHA-256 mismatch"
    );

    let target_blobs = crate::structural_cache::current_blob_ids(request.repo_root)?;
    let base_files = base_report
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect::<BTreeMap<_, _>>();
    ensure!(
        base_files.len() == base_report.files.len(),
        "replay base report contains duplicate file evidence"
    );
    let mut inherited_by_path = BTreeMap::new();
    for file in authorization.inherited_files.iter().cloned() {
        let normalized = crate::lsp_completeness::normalize_repo_relative_path(&file.path)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            normalized == file.path,
            "replay inherited path is not normalized: {}",
            file.path
        );
        ensure!(
            target_blobs.get(&file.path) == Some(&file.blob),
            "replay inherited blob mismatch for {}",
            file.path
        );
        let base_file = base_files
            .get(file.path.as_str())
            .with_context(|| format!("replay base report has no evidence for {}", file.path))?;
        ensure!(
            crate::structural_cache::canonical_json_sha256(*base_file)? == file.base_file_sha256,
            "replay inherited base-file digest mismatch for {}",
            file.path
        );
        ensure!(
            base_file.language.as_deref() == Some(file.language.as_str())
                && base_file
                    .expected_result_ids
                    .iter()
                    .eq(file.expected_result_ids.iter()),
            "replay inherited file/result identity mismatch for {}",
            file.path
        );
        let partition = identity
            .partitions
            .get(&file.language)
            .with_context(|| format!("replay has no current {} partition", file.language))?;
        ensure!(
            partition.signature == file.partition_signature
                && !invalidated.contains(&file.language),
            "replay inherited partition is incompatible for {}",
            file.path
        );
        ensure!(
            inherited_by_path.insert(file.path.clone(), file).is_none(),
            "replay authorization contains duplicate inherited paths"
        );
    }

    Ok(VerifiedStructuralCacheAuthorization {
        authorization,
        inherited_by_path,
        base_report,
    })
}

fn unique_completed_incremental_job(repo_root: &Path) -> Result<EnrichmentJobRecord> {
    let candidates = EnrichmentJobLedger::default()
        .all_jobs(repo_root)
        .into_iter()
        .filter(|job| {
            job.capability == EnrichmentCapability::CallReferences
                && job.scope == EnrichmentScope::ChangedFiles
                && job.trigger == EnrichmentTrigger::IncrementalRefresh
        })
        .collect::<Vec<_>>();
    ensure!(
        candidates.len() == 1,
        "replay requires one incremental LSP job, found {}",
        candidates.len()
    );
    let job = candidates.into_iter().next().expect("length checked");
    ensure!(
        job.state == EnrichmentJobState::Completed,
        "incremental LSP job is not completed"
    );
    ensure!(
        job.failure.is_none(),
        "incremental LSP job records a failure"
    );
    let evidence = job
        .lsp_evidence
        .as_ref()
        .context("incremental LSP job has no evidence")?;
    ensure!(
        !evidence.circuit_open && evidence.detail.is_none(),
        "incremental LSP job is degraded"
    );
    Ok(job)
}

fn validate_replay_work_items(
    records: &[LspWorkItemRecord],
    target_job: &EnrichmentJobRecord,
) -> Result<()> {
    ensure!(!records.is_empty(), "replay work ledger is empty");
    let lower = target_job.created_at.saturating_mul(1_000);
    let upper = target_job
        .completed_at
        .context("completed incremental job has no completion time")?
        .saturating_add(5)
        .saturating_mul(1_000);
    for record in records {
        ensure!(
            record.state == LspWorkItemState::Completed,
            "replay work ledger contains non-completed work"
        );
        ensure!(
            record.created_at_ms >= lower && record.created_at_ms <= upper,
            "replay work ledger contains work outside the incremental job window"
        );
    }
    Ok(())
}

fn validate_retained_checkpoint(
    repo_root: &Path,
    authorization: &VerifiedStructuralCacheAuthorization,
    plan: &IncrementalImpactPlan,
    records: &[LspWorkItemRecord],
    validations: &[LspValidationEvidence],
    nodes: &[Node],
    edges: &[Edge],
) -> Result<CheckpointValidationSummary> {
    let inventory_paths = authorization
        .authorization
        .path_partitions
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    ensure!(
        !inventory_paths.is_empty(),
        "replay target inventory is empty"
    );
    let inherited_paths = plan
        .inherited_paths
        .iter()
        .map(|path| {
            path.to_str()
                .context("replay inherited path is not valid UTF-8")
                .and_then(normalized_relative_path)
        })
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        inherited_paths
            .iter()
            .all(|path| authorization.inherited_by_path.contains_key(path)),
        "replay plan inherits a path without verifier authorization"
    );
    let executed_inventory_paths = inventory_paths
        .difference(&inherited_paths)
        .cloned()
        .collect::<BTreeSet<_>>();
    let node_ids = nodes.iter().map(Node::stable_id).collect::<BTreeSet<_>>();
    let edge_ids = edges.iter().map(Edge::stable_id).collect::<BTreeSet<_>>();
    let graph_result_ids = node_ids
        .iter()
        .chain(edge_ids.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut validations_by_path = BTreeMap::new();
    for validation in validations {
        ensure!(
            validation.schema_version == LSP_VALIDATION_EVIDENCE_SCHEMA_VERSION,
            "replay validation evidence schema mismatch"
        );
        ensure!(
            validation.status != LspValidationStatus::NotValidated
                && validation.method.is_some()
                && validation.detail.is_none(),
            "replay validation evidence contains failed/degraded required work"
        );
        let Some(request_uri) = validation.request_uri.as_deref() else {
            ensure!(
                validation.method.as_deref() == Some("workspace/symbol"),
                "non-file replay validation is not the workspace-symbol probe"
            );
            continue;
        };
        let path = inventory_path_for_uri(request_uri, repo_root, &inventory_paths)?;
        ensure!(
            executed_inventory_paths.contains(&path),
            "replay target validation unexpectedly covers inherited path {path}"
        );
        ensure!(
            authorization.authorization.path_partitions[&path].as_str()
                == validation.language.as_str(),
            "replay validation language does not match target inventory for {path}"
        );
        ensure!(
            validations_by_path
                .insert(path.clone(), validation)
                .is_none(),
            "duplicate replay validation for {path}"
        );
        let capabilities = validation
            .negotiated_capabilities
            .context("file validation is missing negotiated operation capabilities")?;
        ensure!(
            capabilities.document_symbol_provider,
            "file validation did not negotiate document-symbol support for {path}"
        );
        for symbol in validation.document_symbols.iter() {
            ensure!(
                symbol.file.as_deref() == Some(path.as_str()),
                "document-symbol evidence path mismatch for {path}"
            );
            let graph_result_id = symbol
                .graph_result_id
                .as_ref()
                .context("discarded document-symbol response in retained checkpoint")?;
            ensure!(
                node_ids.contains(graph_result_id),
                "document-symbol response is absent after graph reopen: {graph_result_id}"
            );
        }
    }
    ensure!(
        validations_by_path
            .keys()
            .eq(executed_inventory_paths.iter()),
        "retained checkpoint validation does not cover the exact executed target inventory"
    );

    for path in &inherited_paths {
        let base_file = authorization
            .base_report
            .files
            .iter()
            .find(|file| file.path == *path)
            .with_context(|| format!("replay base report has no inherited path {path}"))?;
        ensure!(
            base_file.role.is_included()
                && base_file.language.as_deref().is_some_and(|language| {
                    authorization.authorization.path_partitions[path].as_str() == language
                }),
            "replay inherited readiness evidence is incompatible for {path}"
        );
    }

    let mut observed_result_count = 0_u64;
    let mut persisted_observed_result_count = 0_u64;
    let mut persisted_result_ids = BTreeSet::new();
    let mut discarded_required_result_count = 0_u64;
    for record in records {
        ensure!(
            executed_inventory_paths.contains(&record.file),
            "replay work item is outside the executed target inventory: {}",
            record.file
        );
        ensure!(
            !record.input_hash.is_empty() && !record.requested_operations.is_empty(),
            "replay work item lacks input/operation identity"
        );
        let validation = validations_by_path
            .get(&record.file)
            .with_context(|| format!("no target validation for work item {}", record.file))?;
        let capabilities = validation
            .negotiated_capabilities
            .context("target validation lacks negotiated capabilities")?;
        for operation in &record.requested_operations {
            ensure!(
                operation_supported(operation, &capabilities),
                "required operation {operation} was not negotiated for {}",
                record.file
            );
        }
        let disposition =
            validate_work_item_result_disposition(record, &node_ids, &edge_ids, &graph_result_ids)?;
        observed_result_count += disposition.observed;
        persisted_observed_result_count += disposition.persisted;
        discarded_required_result_count += disposition.discarded;
        persisted_result_ids.extend(record.produced_result_ids.iter().cloned());
    }
    ensure!(
        discarded_required_result_count == 0,
        "retained checkpoint contains discarded required work results"
    );
    // Persisted external/pathless edge endpoints are a supported graph shape;
    // record them for comparison while the unchanged readiness verifier remains
    // authoritative for required-result persistence.
    let unresolved_endpoint_count = edges
        .iter()
        .filter(|edge| {
            !node_ids.contains(&edge.from.to_stable_id())
                || !node_ids.contains(&edge.to.to_stable_id())
        })
        .count() as u64;

    Ok(CheckpointValidationSummary {
        target_inventory_path_count: inventory_paths.len() as u64,
        validated_inventory_path_count: (validations_by_path.len() + inherited_paths.len()) as u64,
        observed_result_count,
        persisted_observed_result_count,
        persisted_result_id_count: persisted_result_ids.len() as u64,
        unresolved_endpoint_count,
        discarded_required_result_count,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkItemResultDisposition {
    observed: u64,
    persisted: u64,
    discarded: u64,
}

fn validate_work_item_result_disposition(
    record: &LspWorkItemRecord,
    node_ids: &BTreeSet<String>,
    edge_ids: &BTreeSet<String>,
    graph_result_ids: &BTreeSet<String>,
) -> Result<WorkItemResultDisposition> {
    ensure!(
        record.requested_operations.len() == 1,
        "replay work item must bind exactly one result-producing operation"
    );
    let operation = &record.requested_operations[0];
    let emitted_result_ids = record
        .output_nodes
        .iter()
        .map(Node::stable_id)
        .chain(record.output_edges.iter().map(Edge::stable_id))
        .collect::<BTreeSet<_>>();
    ensure!(
        emitted_result_ids == record.produced_result_ids,
        "work-item produced-result lineage does not exactly match its graph outputs"
    );
    for node in &record.output_nodes {
        ensure!(
            node_ids.contains(&node.stable_id()),
            "work-item node output is absent after graph reopen: {}",
            node.stable_id()
        );
    }
    for edge in &record.output_edges {
        ensure!(
            edge_ids.contains(&edge.stable_id()),
            "work-item edge output is absent after graph reopen: {}",
            edge.stable_id()
        );
    }
    for result_id in &record.produced_result_ids {
        ensure!(
            graph_result_ids.contains(result_id),
            "produced work result is absent after graph reopen: {result_id}"
        );
    }

    // Each pass-1 record executes exactly one operation. Document-symbol
    // observations map one-for-one to nodes; every other graph enrichment
    // observation maps one-for-one to an edge (with optional extra endpoint
    // nodes). Counting the primary artifact makes partial mapping loss visible
    // without misclassifying materialized call-hierarchy endpoints.
    let persisted = if operation == "document_symbols" {
        record.output_nodes.len() as u64
    } else {
        record.output_edges.len() as u64
    };
    ensure!(
        persisted <= record.observed_result_count,
        "work-item persisted more primary results than it observed"
    );
    let disposition = WorkItemResultDisposition {
        observed: record.observed_result_count,
        persisted,
        discarded: record.observed_result_count - persisted,
    };
    ensure!(
        disposition.observed == disposition.persisted + disposition.discarded,
        "work-item result disposition accounting mismatch"
    );
    ensure!(
        disposition.discarded == 0,
        "work-item discarded {} of {} required observed results",
        disposition.discarded,
        disposition.observed
    );
    Ok(disposition)
}

fn inventory_path_for_uri(
    uri: &str,
    repo_root: &Path,
    inventory_paths: &BTreeSet<String>,
) -> Result<String> {
    let file_path = url::Url::parse(uri)
        .context("invalid retained validation request URI")?
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("retained validation URI is not a file URI"))?;
    let relative = file_path
        .strip_prefix(repo_root)
        .context("retained validation URI is outside the bound target checkout")?;
    let relative = relative
        .to_str()
        .context("retained validation URI path is not valid UTF-8")?;
    let relative = normalized_relative_path(relative)?;
    ensure!(
        inventory_paths.contains(&relative),
        "retained validation URI is absent from the exact target inventory"
    );
    Ok(relative)
}

fn operation_supported(operation: &str, capabilities: &LspNegotiatedCapabilities) -> bool {
    match operation {
        "document_symbols" => capabilities.document_symbol_provider,
        "definitions" => capabilities.definition_provider,
        "references" => capabilities.references_provider,
        "call_hierarchy" => capabilities.call_hierarchy_provider,
        "document_links" => capabilities.document_link_provider,
        "implementations" => capabilities.implementation_provider,
        "code_actions" => capabilities.code_action_provider,
        _ => false,
    }
}

fn diagnostic_execution_plan(
    authorization: &VerifiedStructuralCacheAuthorization,
    nodes: &[Node],
    edges: &[Edge],
) -> IncrementalImpactPlan {
    crate::structural_cache::plan_incremental_impact(authorization, nodes, edges, nodes, edges)
}

fn stale_paths(authorization: &StructuralCacheAuthorization) -> BTreeSet<PathBuf> {
    authorization
        .deleted_paths
        .iter()
        .map(PathBuf::from)
        .chain(
            authorization
                .renamed_paths
                .iter()
                .map(|rename| PathBuf::from(&rename[0])),
        )
        .collect()
}

fn stale_graph_record_counts(
    nodes: &[Node],
    edges: &[Edge],
    stale_paths: &BTreeSet<PathBuf>,
) -> (usize, usize) {
    let nodes = nodes
        .iter()
        .filter(|node| stale_paths.contains(&node.id.file))
        .count();
    let edges = edges
        .iter()
        .filter(|edge| stale_paths.contains(&edge.from.file) || stale_paths.contains(&edge.to.file))
        .count();
    (nodes, edges)
}

fn verify_retained_evidence(
    cache_root: &Path,
    receipt_path: &Path,
    expected_receipt_sha256: &str,
) -> Result<RetainedFailureReceipt> {
    let receipt_metadata = fs::symlink_metadata(receipt_path)
        .with_context(|| format!("inspect failure receipt {}", receipt_path.display()))?;
    ensure!(
        receipt_metadata.file_type().is_file() && !receipt_metadata.file_type().is_symlink(),
        "failure receipt must be a regular non-symlink file"
    );
    ensure!(
        sha256_file(receipt_path)? == expected_receipt_sha256,
        "failure receipt SHA-256 mismatch"
    );
    let receipt: RetainedFailureReceipt = serde_json::from_slice(&fs::read(receipt_path)?)
        .context("invalid retained failure receipt")?;
    let mut expected = BTreeMap::new();
    for evidence in &receipt.evidence {
        let normalized = normalized_relative_path(&evidence.cache_path)?;
        require_sha256(&evidence.sha256, "retained evidence SHA-256")?;
        ensure!(
            expected
                .insert(normalized, (evidence.size_bytes, evidence.sha256.clone()))
                .is_none(),
            "failure receipt contains duplicate cache evidence"
        );
    }
    let actual = regular_cache_files(cache_root)?;
    ensure!(
        actual.keys().eq(expected.keys()),
        "diagnostic cache copy is partial or contains undeclared files"
    );
    for (relative, path) in actual {
        let (expected_size, expected_sha256) = &expected[&relative];
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.len() == *expected_size,
            "retained evidence size mismatch for {relative}"
        );
        ensure!(
            sha256_file(&path)? == *expected_sha256,
            "retained evidence SHA-256 mismatch for {relative}"
        );
    }
    Ok(receipt)
}

fn regular_cache_files(root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let metadata = fs::symlink_metadata(root)
        .with_context(|| format!("inspect diagnostic cache root {}", root.display()))?;
    ensure!(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        "diagnostic cache root must be a regular directory"
    );
    let mut files = BTreeMap::new();
    collect_regular_cache_files(root, root, &mut files)?;
    Ok(files)
}

fn collect_regular_cache_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, PathBuf>,
) -> Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            !metadata.file_type().is_symlink(),
            "diagnostic cache contains a symlink: {}",
            path.display()
        );
        if metadata.file_type().is_dir() {
            collect_regular_cache_files(root, &path, files)?;
        } else if metadata.file_type().is_file() {
            let relative = path.strip_prefix(root).expect("descendant");
            let relative = normalized_relative_path(&relative.to_string_lossy())?;
            ensure!(
                files.insert(relative, path).is_none(),
                "duplicate diagnostic cache member"
            );
        } else {
            bail!(
                "diagnostic cache contains a special file: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn normalized_relative_path(value: &str) -> Result<String> {
    let path = Path::new(value);
    ensure!(
        !path.is_absolute() && !value.is_empty() && !value.contains('\\'),
        "cache member path must be a normalized relative POSIX path"
    );
    ensure!(
        value
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != ".."),
        "cache member path contains traversal or non-normal components"
    );
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "cache member path contains traversal or non-normal components"
    );
    let normalized = path.to_string_lossy().to_string();
    ensure!(normalized == value, "cache member path is not normalized");
    Ok(normalized)
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn require_sha256(value: &str, label: &str) -> Result<()> {
    ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{label} must be a 64-character hexadecimal SHA-256"
    );
    Ok(())
}

fn require_git_oid(value: &str, label: &str) -> Result<()> {
    ensure!(
        value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{label} must be a 40-character hexadecimal Git object ID"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_receipt_contract_is_non_publishable_and_zero_lsp() {
        let producer = StructuralProducerIdentity {
            producer_commit: "d".repeat(40),
            package_version: "0.2.10".to_string(),
            binary_sha256: "a".repeat(64),
            graph_schema_version: 26,
            graph_schema_signature: "b".repeat(64),
            completeness_schema_version: 6,
            work_item_schema_version: 4,
            validation_evidence_schema_version: 5,
        };
        let receipt = StructuralCacheReplayReceipt {
            schema_version: 1,
            diagnostic_only: true,
            publishable: false,
            checkout_rebuilt: false,
            lsp_calls: 0,
            archive_created: false,
            catalog_updated: false,
            failure_receipt_sha256: "a".repeat(64),
            failure_digest: "b".repeat(64),
            authorization_sha256: "c".repeat(64),
            source_producer_commit: "d".repeat(40),
            replay_producer_commit: "e".repeat(40),
            source_producer: producer.clone(),
            replay_producer: StructuralProducerIdentity {
                producer_commit: "e".repeat(40),
                binary_sha256: "c".repeat(64),
                ..producer
            },
            target_commit: "f".repeat(40),
            target_tree: "1".repeat(40),
            target_tree_source: "copied_retained_checkout_and_verified_authorization".to_string(),
            source_checkout_identity_verified: true,
            source_tree_diff_replayed: false,
            source_rescanned: false,
            full_target_readiness_recomputed: true,
            incremental_enrichment_job_id: "call_references-target".to_string(),
            pass1_job_ids: vec!["lsp-pass1-target".to_string()],
            initial_node_count: 2,
            initial_edge_count: 1,
            stale_path_count: 1,
            stale_node_count_before: 1,
            stale_edge_count_before: 1,
            removed_node_count: 1,
            removed_edge_count: 1,
            final_node_count: 1,
            final_edge_count: 0,
            completed_work_item_count: 1,
            executed_operation_count: 1,
            readiness_validation_request_count: 1,
            base_completeness_digest: "2".repeat(64),
            target_inventory_path_count: 1,
            validated_inventory_path_count: 1,
            observed_result_count: 1,
            persisted_observed_result_count: 1,
            persisted_result_id_count: 1,
            unresolved_endpoint_count: 0,
            discarded_required_result_count: 0,
            checkpoint_validation_digest: "3".repeat(64),
            diagnostic_checkpoint_validation_passed: true,
            target_completeness_digest: "4".repeat(64),
            coverage_violation_count: 0,
            compatibility_violation_count: 0,
            full_target_ready: true,
        };
        let value = serde_json::to_value(receipt).unwrap();
        assert_eq!(value["diagnostic_only"], true);
        assert_eq!(value["publishable"], false);
        assert_eq!(value["checkout_rebuilt"], false);
        assert_eq!(value["lsp_calls"], 0);
        assert_eq!(value["archive_created"], false);
        assert_eq!(value["catalog_updated"], false);
    }

    #[test]
    fn cache_member_paths_reject_traversal() {
        assert!(normalized_relative_path("lance/data.bin").is_ok());
        assert!(normalized_relative_path("../outside").is_err());
        assert!(normalized_relative_path("/absolute").is_err());
        assert!(normalized_relative_path("lance/./data.bin").is_err());
    }

    #[test]
    fn validation_uri_binds_to_the_exact_checkout_relative_path() {
        let repo = tempfile::tempdir().unwrap();
        let inventory = BTreeSet::from(["setup.py".to_string(), "package/setup.py".to_string()]);
        let nested = repo.path().join("package/setup.py");
        let nested_uri = url::Url::from_file_path(&nested).unwrap().to_string();

        assert_eq!(
            inventory_path_for_uri(&nested_uri, repo.path(), &inventory).unwrap(),
            "package/setup.py"
        );

        let outside = repo.path().parent().unwrap().join("setup.py");
        let outside_uri = url::Url::from_file_path(outside).unwrap().to_string();
        assert!(inventory_path_for_uri(&outside_uri, repo.path(), &inventory).is_err());
    }

    #[test]
    fn mixed_retained_and_discarded_work_results_fail_closed() {
        use crate::graph::{Confidence, EdgeKind, ExtractionSource, NodeId, NodeKind};

        let from = NodeId {
            root: "fixture".to_string(),
            file: PathBuf::from("src/a.py"),
            name: "caller".to_string(),
            kind: NodeKind::Function,
        };
        let to = NodeId {
            root: "fixture".to_string(),
            file: PathBuf::from("src/b.py"),
            name: "target".to_string(),
            kind: NodeKind::Function,
        };
        let edge = Edge {
            from,
            to,
            kind: EdgeKind::ReferencedBy,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let edge_id = edge.stable_id();
        let record = LspWorkItemRecord {
            requested_operations: vec!["references".to_string()],
            output_edges: vec![edge],
            produced_result_ids: BTreeSet::from([edge_id.clone()]),
            observed_result_count: 2,
            ..LspWorkItemRecord::default()
        };

        let error = validate_work_item_result_disposition(
            &record,
            &BTreeSet::new(),
            &BTreeSet::from([edge_id.clone()]),
            &BTreeSet::from([edge_id]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("discarded 1 of 2"));
    }
}
