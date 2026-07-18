//! Background enrichment: LSP enrichment, embedding pipeline, and background scanner.
//!
//! ## Module structure
//!
//! The background scanner stages are extracted into `bg_scanner`:
//! - `scan_roots()` -- resolve workspace roots, detect file changes
//! - `update_graph()` -- apply changes, run enrichment pipeline
//! - `persist_deltas()` -- write to LanceDB, commit scanner state
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::embed::EmbeddingIndex;
use crate::graph::{Edge, Node};
use crate::roots::{RootConfig, WorkspaceConfig};
use crate::scanner::Scanner;

use super::changed_file_plan::discover_and_plan_changed_files_with_broad_references;
use super::enrichment_jobs::{
    BroadReferenceBudget, EnrichmentCapability, EnrichmentJobState, EnrichmentScope,
    EnrichmentTrigger, JobStart, LspEvidenceCoverage, LspEvidenceReadiness, ScanEnrichmentOptions,
};
use super::operation_report::{
    CapabilityState, OperationKind, OperationReport, OperationTrigger, OutputReport, PhaseKind,
    PhaseReport, add_scan_degradation_and_next_steps, embedding_capability_from_availability,
    lsp_capability_from_status, scan_capability_reports,
};
use super::state::GraphState;
use super::store::{persist_graph_incremental, persist_graph_to_lance};
use super::{PipelineResult, RnaHandler};

/// Check if a cached graph is missing enrichment passes output that should exist.
///
/// Returns `true` if the cache appears stale: it has Import nodes (so framework
/// detection should produce results) but zero `NodeKind::Other("framework")` nodes.
/// This catches caches built before the enrichment pipeline was wired via the event
/// bus (pre-v2-rc) or where a persist race dropped framework nodes.
///
/// When this returns `true`, the caller should re-run the enrichment pipeline
/// instead of serving the stale cache.
pub(crate) fn cache_needs_enrichment(nodes: &[Node]) -> bool {
    let has_imports = nodes
        .iter()
        .any(|n| n.id.kind == crate::graph::NodeKind::Import);
    if !has_imports {
        return false; // No imports => no framework detection possible, cache is fine.
    }
    let has_framework_nodes = nodes
        .iter()
        .any(|n| matches!(&n.id.kind, crate::graph::NodeKind::Other(s) if s == "framework"));
    // If there are imports but no framework nodes, AND the imports match at least one
    // framework rule, the cache is missing enrichment output.
    if has_framework_nodes {
        return false;
    }
    // Quick check: do any imports match known framework patterns?
    let result = crate::extract::framework_detection::framework_detection_pass(nodes, "check");
    !result.detected_frameworks.is_empty()
}

fn lsp_abort_failures_for_slugs(
    stats: &crate::extract::scan_stats::ScanStats,
    participating_slugs: &HashSet<String>,
) -> Vec<String> {
    stats
        .lsp_stats
        .iter()
        .filter(|(slug, _)| participating_slugs.contains(*slug))
        .flat_map(|(slug, by_language)| {
            by_language.iter().filter_map(move |(language, stat)| {
                if stat.aborted {
                    Some(format!(
                        "{slug}/{language} via {}: {} error(s), aborted={}",
                        stat.server_name, stat.error_count, stat.aborted
                    ))
                } else {
                    None
                }
            })
        })
        .collect()
}

#[derive(Debug, Clone)]
struct PipelineReportInput {
    operation: OperationKind,
    enrichment: ScanEnrichmentOptions,
    duration: Duration,
    symbol_count: usize,
    edge_count: usize,
    file_count: usize,
    lsp_edge_count: usize,
    lsp_state: CapabilityState,
    lsp_detail: Option<String>,
    embedding_count: usize,
    embeddings_attached: bool,
    phases: Vec<PhaseReport>,
    related_job_ids: Vec<String>,
    business_context: crate::business_context::BusinessContextAdmission,
}

fn build_pipeline_operation_report(
    repo_root: &std::path::Path,
    input: PipelineReportInput,
) -> OperationReport {
    let mut report =
        OperationReport::new(input.operation, OperationTrigger::ForegroundScan, repo_root)
            .with_scope("repo")
            .complete(input.duration);
    if let Some(completed_at) = report.completed_at {
        report.started_at = completed_at.saturating_sub(input.duration.as_secs());
    }
    report.outputs = OutputReport {
        symbol_count: Some(input.symbol_count),
        edge_count: Some(input.edge_count),
        file_count: (input.file_count > 0).then_some(input.file_count),
        embedding_count: Some(input.embedding_count),
        lsp_edge_count: Some(input.lsp_edge_count),
    };
    for phase in input.phases {
        report.add_phase(phase);
    }
    let (embedding_state, embedding_detail) =
        embedding_capability_from_availability(input.enrichment, input.embeddings_attached);
    for capability in scan_capability_reports(
        input.enrichment,
        embedding_state,
        embedding_detail,
        input.lsp_state,
        input.lsp_detail,
        Some("repo".to_string()),
    ) {
        report.add_capability(capability);
    }
    add_scan_degradation_and_next_steps(
        &mut report,
        repo_root,
        input.enrichment,
        embedding_state,
        input.lsp_state,
    );
    report.related_job_ids = input.related_job_ids;
    report.record_business_context(&input.business_context);
    report
}

type LspBusOutput = (
    Vec<Node>,
    Vec<Edge>,
    HashSet<String>,
    Vec<String>,
    Vec<crate::extract::scan_stats::LspValidationEvidence>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichmentContinuation {
    Disabled,
    SpawnBackground,
    RunToCompletion,
}

impl EnrichmentContinuation {
    fn enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

fn should_continue_lsp_enrichment(
    scope: &EnrichmentScope,
    continuation: EnrichmentContinuation,
) -> bool {
    continuation.enabled()
        && !matches!(
            scope,
            EnrichmentScope::Repo
                | EnrichmentScope::ChangedFiles
                | EnrichmentScope::TargetSymbols(_)
                | EnrichmentScope::TaskRelevant { .. }
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LspBudget {
    pub max_duration: Duration,
}

impl LspBudget {
    pub(crate) fn from_env() -> Self {
        let millis = std::env::var("RNA_LSP_JOB_TIMEOUT_MS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|millis| *millis > 0)
            .unwrap_or(30 * 60 * 1000);
        Self {
            max_duration: Duration::from_millis(millis),
        }
    }
}

struct LspEnrichmentRun {
    edge_count: usize,
    job_id: String,
}

struct ForegroundLspRequest {
    scope: EnrichmentScope,
    trigger: EnrichmentTrigger,
    dirty_slugs: Option<HashSet<String>>,
    node_filter: Option<Arc<HashSet<String>>>,
    fail_on_lsp_error: bool,
    broad_reference_budget: Option<Arc<crate::extract::lsp::LspBroadReferenceBudget>>,
    declared_node_count: usize,
}

#[derive(Debug)]
struct ScopedLspPersistenceDelta {
    upsert_nodes: Vec<Node>,
    upsert_edges: Vec<Edge>,
    deleted_edge_ids: Vec<String>,
}

fn is_scoped_lsp_edge(edge: &Edge, node_filter: &HashSet<String>) -> bool {
    edge.source == crate::graph::ExtractionSource::Lsp
        && (node_filter.contains(&edge.from.to_stable_id())
            || node_filter.contains(&edge.to.to_stable_id()))
}

fn remove_existing_scoped_lsp_edges(
    edges: &mut Vec<Edge>,
    node_filter: &HashSet<String>,
) -> Vec<String> {
    let mut removed = Vec::new();
    edges.retain(|edge| {
        if is_scoped_lsp_edge(edge, node_filter) {
            removed.push(edge.stable_id());
            false
        } else {
            true
        }
    });
    removed.sort();
    removed.dedup();
    removed
}

fn purge_existing_scoped_lsp_output(
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    node_filter: &HashSet<String>,
    file_filter: &HashSet<PathBuf>,
) -> Vec<String> {
    let directly_impacted_lsp_node_ids = nodes
        .iter()
        .filter(|node| {
            node.source == crate::graph::ExtractionSource::Lsp
                && (node_filter.contains(&node.stable_id()) || file_filter.contains(&node.id.file))
        })
        .map(Node::stable_id)
        .collect::<HashSet<_>>();
    let is_impacted_edge = |edge: &Edge| {
        directly_impacted_lsp_node_ids.contains(&edge.from.to_stable_id())
            || directly_impacted_lsp_node_ids.contains(&edge.to.to_stable_id())
            || is_scoped_lsp_edge(edge, node_filter)
    };
    let impacted_lsp_node_ids = edges
        .iter()
        .filter(|edge| is_impacted_edge(edge))
        .flat_map(|edge| [edge.from.to_stable_id(), edge.to.to_stable_id()])
        .collect::<HashSet<_>>();
    let mut removed_edge_ids = Vec::new();
    edges.retain(|edge| {
        if is_impacted_edge(edge) {
            removed_edge_ids.push(edge.stable_id());
            false
        } else {
            true
        }
    });
    removed_edge_ids.sort();
    removed_edge_ids.dedup();
    let retained_endpoint_ids = edges
        .iter()
        .flat_map(|edge| [edge.from.to_stable_id(), edge.to.to_stable_id()])
        .collect::<HashSet<_>>();
    nodes.retain(|node| {
        if node.source != crate::graph::ExtractionSource::Lsp {
            return true;
        }
        let stable_id = node.stable_id();
        if directly_impacted_lsp_node_ids.contains(&stable_id) {
            return false;
        }
        let impacted = impacted_lsp_node_ids.contains(&stable_id);
        !impacted || retained_endpoint_ids.contains(&stable_id)
    });
    removed_edge_ids
}

fn dedup_edges_preserving_lsp_evidence(edges: &mut Vec<Edge>) {
    let mut positions = std::collections::HashMap::<String, usize>::new();
    let mut deduplicated: Vec<Edge> = Vec::with_capacity(edges.len());
    for edge in edges.drain(..) {
        let stable_id = edge.stable_id();
        if let Some(&position) = positions.get(&stable_id) {
            if edge.source == crate::graph::ExtractionSource::Lsp
                && deduplicated[position].source != crate::graph::ExtractionSource::Lsp
            {
                deduplicated[position] = edge;
            }
        } else {
            positions.insert(stable_id, deduplicated.len());
            deduplicated.push(edge);
        }
    }
    *edges = deduplicated;
}

fn scoped_lsp_persistence_delta(
    enriched_nodes: &[Node],
    enriched_edges: &[Edge],
    node_filter: &HashSet<String>,
    existing_node_ids: &HashSet<String>,
    existing_edge_ids: &HashSet<String>,
    deleted_edge_ids: Vec<String>,
) -> ScopedLspPersistenceDelta {
    let mut upsert_edges = enriched_edges
        .iter()
        .filter(|edge| {
            is_scoped_lsp_edge(edge, node_filter) || !existing_edge_ids.contains(&edge.stable_id())
        })
        .cloned()
        .collect::<Vec<_>>();
    upsert_edges.sort_by_key(Edge::stable_id);

    let endpoint_ids = upsert_edges
        .iter()
        .flat_map(|edge| [edge.from.to_stable_id(), edge.to.to_stable_id()])
        .collect::<HashSet<_>>();
    let mut upsert_nodes = enriched_nodes
        .iter()
        .filter(|node| {
            let stable_id = node.stable_id();
            !existing_node_ids.contains(&stable_id)
                || (node.source == crate::graph::ExtractionSource::Lsp
                    && endpoint_ids.contains(&stable_id))
        })
        .cloned()
        .collect::<Vec<_>>();
    upsert_nodes.sort_by_key(Node::stable_id);

    ScopedLspPersistenceDelta {
        upsert_nodes,
        upsert_edges,
        deleted_edge_ids,
    }
}

#[derive(Debug)]
struct LspPipelineFailure(anyhow::Error);

impl LspPipelineFailure {
    fn is_timeout(&self) -> bool {
        false
    }
}

impl std::fmt::Display for LspPipelineFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for LspPipelineFailure {}

struct LspPipelineInput {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    root_pairs: Vec<(String, PathBuf)>,
    primary_slug: String,
    repo_root: PathBuf,
    scan_stats: Arc<std::sync::RwLock<crate::extract::scan_stats::ScanStats>>,
    skip_lsp: bool,
    dirty_slugs: Option<HashSet<String>>,
    lsp_node_filter: Option<Arc<HashSet<String>>>,
    file_readiness_filter: Option<Arc<HashSet<PathBuf>>>,
    broad_reference_budget: Option<Arc<crate::extract::lsp::LspBroadReferenceBudget>>,
}

pub(super) fn lsp_evidence(
    readiness: LspEvidenceReadiness,
    scope: &str,
    declared_node_count: usize,
    budget: Option<&crate::extract::lsp::LspBroadReferenceBudgetSnapshot>,
    detail: Option<String>,
    validations: Vec<crate::extract::scan_stats::LspValidationEvidence>,
) -> LspEvidenceCoverage {
    LspEvidenceCoverage {
        readiness,
        scope: scope.to_string(),
        declared_node_count,
        max_requests: budget.map(|snapshot| snapshot.max_requests),
        max_duration_ms: budget.map(|snapshot| snapshot.max_duration_ms),
        scheduled_requests: budget.map_or(0, |snapshot| snapshot.scheduled_requests),
        elapsed_ms: budget.map_or(0, |snapshot| snapshot.elapsed_ms),
        circuit_open: budget.is_some_and(|snapshot| snapshot.circuit_open),
        detail,
        validations,
    }
}

fn normalized_scope_path(value: &str) -> String {
    value.trim().trim_start_matches("./").replace('\\', "/")
}

fn resolve_symbol_selector(all_nodes: &[Node], selector: &str) -> anyhow::Result<String> {
    let mut exact = all_nodes
        .iter()
        .filter(|node| node.id.root != "external" && node.stable_id() == selector)
        .map(Node::stable_id)
        .collect::<Vec<_>>();
    if exact.len() == 1 {
        return Ok(exact.remove(0));
    }

    let mut by_name = all_nodes
        .iter()
        .filter(|node| node.id.root != "external" && node.id.name == selector)
        .map(Node::stable_id)
        .collect::<Vec<_>>();
    by_name.sort();
    by_name.dedup();
    match by_name.as_slice() {
        [stable_id] => Ok(stable_id.clone()),
        [] => anyhow::bail!("target symbol `{selector}` did not match a cached node"),
        _ => anyhow::bail!(
            "target symbol `{selector}` is ambiguous across {} cached nodes; pass a stable node ID",
            by_name.len()
        ),
    }
}

fn plan_explicit_lsp_scope(
    all_nodes: &[Node],
    scope: &EnrichmentScope,
) -> anyhow::Result<Arc<HashSet<String>>> {
    let mut planned = std::collections::BTreeSet::new();
    match scope {
        EnrichmentScope::TargetSymbols(symbols) => {
            anyhow::ensure!(!symbols.is_empty(), "target-symbol scope cannot be empty");
            for selector in symbols {
                planned.insert(resolve_symbol_selector(all_nodes, selector)?);
            }
        }
        EnrichmentScope::TaskRelevant { files, symbols } => {
            anyhow::ensure!(
                !files.is_empty() || !symbols.is_empty(),
                "task-relevant scope requires at least one file or symbol"
            );
            for file in files {
                let normalized = normalized_scope_path(file);
                let matching = all_nodes
                    .iter()
                    .filter(|node| {
                        node.id.root != "external"
                            && normalized_scope_path(&node.id.file.to_string_lossy()) == normalized
                    })
                    .map(Node::stable_id)
                    .collect::<Vec<_>>();
                anyhow::ensure!(
                    !matching.is_empty(),
                    "task-relevant file `{file}` did not match a cached node"
                );
                planned.extend(matching);
            }
            for selector in symbols {
                planned.insert(resolve_symbol_selector(all_nodes, selector)?);
            }
        }
        _ => anyhow::bail!(
            "scope {} is not an explicit target/task LSP scope",
            scope.stable_key()
        ),
    }
    Ok(Arc::new(planned.into_iter().collect()))
}

async fn emit_lsp_pipeline_with_budget(
    input: LspPipelineInput,
) -> Result<LspBusOutput, LspPipelineFailure> {
    let fut = crate::extract::consumers::emit_enrichment_pipeline_with_validations(
        input.nodes,
        input.edges,
        input.root_pairs,
        input.primary_slug,
        input.repo_root,
        crate::extract::consumers::BusOptions {
            business_context: Default::default(),
            scan_stats: Some(input.scan_stats),
            embed_idx: None,
            lance_repo_root: None,
            skip_lsp: input.skip_lsp,
            file_readiness: !input.skip_lsp
                && (input.lsp_node_filter.is_none() || input.file_readiness_filter.is_some()),
            file_readiness_filter: input.file_readiness_filter,
            lsp_node_filter: input.lsp_node_filter,
            broad_reference_budget: input.broad_reference_budget,
        },
        input.dirty_slugs,
    );

    // The LSP work-item/pass layer owns its timeout and abort diagnostics. Wrapping the
    // entire event bus in `timeout` drops the future before AllEnrichmentsDone and
    // PassesComplete can preserve partial output, violating the finalization contract.
    fut.await.map_err(LspPipelineFailure)
}

impl RnaHandler {
    /// Spawn the background scanner task (event-driven + 15min heartbeat, worktree-aware).
    ///
    /// The loop calls three extracted stage functions per tick:
    /// 1. `bg_scanner::scan_roots()` -- resolve workspace roots, detect file changes
    /// 2. `bg_scanner::update_graph()` -- apply changes, run enrichment pipeline
    /// 3. `bg_scanner::persist_deltas()` -- write to LanceDB, commit scanner state
    pub(crate) fn spawn_background_scanner(&self) {
        let graph = Arc::clone(&self.graph);
        let repo_root = self.repo_root.clone();
        let business_context = self.business_context.clone();
        let lance_write_lock = Arc::clone(&self.lance_write_lock);
        let scan_stats = Arc::clone(&self.scan_stats);
        let lsp_status = Arc::clone(&self.lsp_status);
        let enrichment_jobs = Arc::clone(&self.enrichment_jobs);
        tokio::spawn(async move {
            // Seed from the current resolved roots so the first tick doesn't
            // misidentify every root as "new".
            let mut prev_root_slugs: std::collections::HashSet<String> = WorkspaceConfig::load()
                .with_primary_root(repo_root.clone())
                .with_worktrees(&repo_root)
                .with_claude_memory(&repo_root)
                .with_agent_memories(&repo_root)
                .with_declared_roots(&repo_root)
                .resolved_roots()
                .into_iter()
                .map(|r| r.slug)
                .collect();

            // HEAD-change detection state.
            let mut last_head_oid: Option<git2::Oid> = None;
            let mut last_fetch_head_mtime: Option<std::time::SystemTime> = None;

            loop {
                // Check for HEAD or FETCH_HEAD changes before waiting.
                let head_changed = {
                    match git2::Repository::open(&repo_root) {
                        Ok(repo) => match repo.head().and_then(|h| h.peel_to_commit()) {
                            Ok(commit) => {
                                let oid = commit.id();
                                let changed = last_head_oid.is_some_and(|prev| prev != oid);
                                last_head_oid = Some(oid);
                                changed
                            }
                            Err(_) => false,
                        },
                        Err(_) => false,
                    }
                };

                let fetch_head_changed = {
                    let fetch_head_path = repo_root.join(".git").join("FETCH_HEAD");
                    match std::fs::metadata(&fetch_head_path).and_then(|m| m.modified()) {
                        Ok(mtime) => {
                            let changed = last_fetch_head_mtime.is_some_and(|prev| prev != mtime);
                            last_fetch_head_mtime = Some(mtime);
                            changed
                        }
                        Err(_) => false,
                    }
                };

                if head_changed {
                    tracing::info!("HEAD changed -- triggering immediate background scan");
                } else if fetch_head_changed {
                    tracing::info!("FETCH_HEAD changed -- triggering immediate background scan");
                } else {
                    tokio::time::sleep(tokio::time::Duration::from_secs(900)).await;
                }

                // Stage 1: scan roots for file changes.
                let mut scan_result = super::bg_scanner::scan_roots(&repo_root, &prev_root_slugs);

                if !scan_result.has_changes && scan_result.removed_slugs.is_empty() {
                    prev_root_slugs = scan_result.current_root_slugs;
                    continue;
                }

                // Stage 2: update graph (lock-free via ArcSwap).
                let current_snap = graph.load_full();
                if let Some(ref current_gs) = *current_snap {
                    let mut graph_state = (**current_gs).clone();

                    let (lance_deltas, diagnostics) = super::bg_scanner::update_graph(
                        &mut graph_state,
                        &mut scan_result,
                        &repo_root,
                        &scan_stats,
                        &business_context,
                    )
                    .await;

                    let degraded_detail = (!diagnostics.is_empty()).then(|| diagnostics.join("; "));
                    let degraded_job_id = if degraded_detail.is_some() {
                        match enrichment_jobs.begin_job(
                            &repo_root,
                            EnrichmentCapability::CallReferences,
                            EnrichmentScope::ChangedFiles,
                            EnrichmentTrigger::BackgroundScan,
                            None,
                        ) {
                            Ok(JobStart::Started(job)) => Some(job.job_id),
                            Ok(JobStart::Joined { existing_job_id }) => Some(existing_job_id),
                            Err(error) => {
                                tracing::error!(
                                    "Background scan: failed to begin durable degraded LSP job: {error:#}"
                                );
                                None
                            }
                        }
                    } else {
                        None
                    };
                    if let Some(job_id) = degraded_job_id.as_deref() {
                        enrichment_jobs.mark_persisting(
                            &repo_root,
                            job_id,
                            graph_state.nodes.len(),
                            graph_state.edges.len(),
                        );
                    }

                    // Atomic swap: publish the new graph state.
                    graph.store(Arc::new(Some(Arc::new(graph_state))));

                    // Stage 3: persist deltas to LanceDB.
                    let persist_succeeded = super::bg_scanner::persist_deltas(
                        lance_deltas,
                        &scan_result.per_root_scans,
                        &scan_result.removed_slugs,
                        &repo_root,
                        &graph,
                        &lance_write_lock,
                    )
                    .await;

                    if let Some(detail) = degraded_detail.as_deref() {
                        let persisted_snapshot = graph.load_full();
                        let (node_count, edge_count, lsp_edge_count) = persisted_snapshot
                            .as_ref()
                            .as_ref()
                            .map(|graph_state| {
                                (
                                    graph_state.nodes.len(),
                                    graph_state.edges.len(),
                                    graph_state
                                        .edges
                                        .iter()
                                        .filter(|edge| {
                                            edge.source == crate::graph::ExtractionSource::Lsp
                                                && matches!(
                                                    edge.kind,
                                                    crate::graph::EdgeKind::Calls
                                                )
                                        })
                                        .count(),
                                )
                            })
                            .unwrap_or((0, 0, 0));
                        if persist_succeeded && degraded_job_id.is_some() {
                            let existing_coverage = lsp_status.coverage_edge_count();
                            lsp_status.set_degraded_scoped(
                                lsp_edge_count,
                                existing_coverage,
                                EnrichmentScope::ChangedFiles.stable_key(),
                                detail,
                            );
                            super::sentinel::clear_lsp_sentinel(&repo_root);
                            if let Some(job_id) = degraded_job_id.as_deref() {
                                enrichment_jobs.mark_degraded(
                                    &repo_root, job_id, node_count, edge_count, detail,
                                );
                            }
                        } else if !persist_succeeded
                            && let Some(job_id) = degraded_job_id.as_deref()
                        {
                            enrichment_jobs.mark_failed(
                                &repo_root,
                                job_id,
                                "degraded LSP output was not durably persisted",
                            );
                        } else if persist_succeeded {
                            // The graph is durable, but readiness cannot be claimed without a
                            // matching durable job record. Keep the sentinel absent so a later
                            // process retries enrichment instead of trusting process-local state.
                            super::sentinel::clear_lsp_sentinel(&repo_root);
                            tracing::error!(
                                "Background scan: degraded LSP output persisted without a durable job record; readiness left unchanged"
                            );
                        }
                    }
                }

                prev_root_slugs = scan_result.current_root_slugs;
            }
        });
        tracing::info!(
            "Background scanner started (event-driven + 15min heartbeat, worktree-aware)"
        );
    }

    /// Spawn background LSP enrichment after the initial graph build returns (#574).
    ///
    /// The initial `build_full_graph_inner` runs with `skip_lsp=true` so it returns
    /// in seconds (tree-sitter + non-LSP passes only). This method spawns LSP enrichment
    /// in the background: when complete, it ArcSwaps the fully enriched graph and
    /// re-persists to LanceDB with LSP edges.
    ///
    /// This restores the v0.1.14 behavior where `build_full_graph_inner` returned
    /// immediately and LSP ran via `spawn_background_enrichment`.
    pub(crate) fn spawn_background_lsp_enrichment(
        &self,
        nodes: Vec<crate::graph::Node>,
        edges: Vec<crate::graph::Edge>,
        dirty_slugs: std::collections::HashSet<String>,
        detected_frameworks: std::collections::HashSet<String>,
    ) -> tokio::task::JoinHandle<()> {
        let repo_root = self.repo_root.clone();
        let graph_arc = Arc::clone(&self.graph);
        let lsp_status = Arc::clone(&self.lsp_status);
        let scan_stats = Arc::clone(&self.scan_stats);
        let lance_write_lock = Arc::clone(&self.lance_write_lock);
        let job_id = match self.enrichment_jobs.begin_job(
            &self.repo_root,
            EnrichmentCapability::CallReferences,
            EnrichmentScope::ChangedFiles,
            EnrichmentTrigger::BackgroundScan,
            None,
        ) {
            Ok(JobStart::Started(job)) => job.job_id,
            Ok(JobStart::Joined { existing_job_id }) => {
                tracing::info!(
                    "[background-lsp] joining active LSP job {}",
                    existing_job_id
                );
                return tokio::spawn(async {});
            }
            Err(e) => {
                tracing::warn!("[background-lsp] failed to begin LSP job: {}", e);
                return tokio::spawn(async {});
            }
        };
        let jobs = Arc::clone(&self.enrichment_jobs);

        tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            jobs.mark_running(&repo_root, &job_id, "lsp");
            tracing::info!(
                "[background-lsp] Starting LSP enrichment: {} nodes, {} edges",
                nodes.len(),
                edges.len()
            );

            // Build root pairs for the enrichment pipeline
            let workspace = crate::roots::WorkspaceConfig::load()
                .with_primary_root(repo_root.clone())
                .with_worktrees(&repo_root)
                .with_declared_roots(&repo_root);
            let root_pairs: Vec<(String, std::path::PathBuf)> = workspace
                .resolved_roots()
                .iter()
                .map(|r| (r.slug.clone(), r.path.clone()))
                .collect();
            let primary_slug = crate::roots::RootConfig::code_project(repo_root.clone()).slug();

            // Run enrichment pipeline WITH LSP (skip_lsp=false).
            let result = emit_lsp_pipeline_with_budget(LspPipelineInput {
                nodes,
                edges,
                root_pairs,
                primary_slug: primary_slug.clone(),
                repo_root: repo_root.clone(),
                scan_stats: Arc::clone(&scan_stats),
                skip_lsp: false,
                dirty_slugs: Some(dirty_slugs),
                lsp_node_filter: None,
                file_readiness_filter: None,
                broad_reference_budget: None,
            })
            .await;

            match result {
                Ok((
                    mut enriched_nodes,
                    mut enriched_edges,
                    enriched_frameworks,
                    diagnostics,
                    validations,
                )) => {
                    // Update LSP status
                    let lsp_edge_count = enriched_edges
                        .iter()
                        .filter(|e| e.source == crate::graph::ExtractionSource::Lsp)
                        .count();
                    let lsp_call_edge_count = enriched_edges
                        .iter()
                        .filter(|e| {
                            e.source == crate::graph::ExtractionSource::Lsp
                                && matches!(e.kind, crate::graph::EdgeKind::Calls)
                        })
                        .count();
                    jobs.mark_progress(
                        &repo_root,
                        &job_id,
                        "lsp_edges",
                        lsp_call_edge_count,
                        Some(lsp_edge_count),
                    );
                    let degraded_detail = (!diagnostics.is_empty()).then(|| diagnostics.join("; "));
                    if let Some(detail) = degraded_detail.as_deref() {
                        let existing_coverage = lsp_status.coverage_edge_count();
                        lsp_status.set_degraded_scoped(
                            lsp_call_edge_count,
                            existing_coverage,
                            EnrichmentScope::ChangedFiles.stable_key(),
                            detail,
                        );
                    } else {
                        let existing_coverage = lsp_status.coverage_edge_count();
                        lsp_status.set_complete_scoped(
                            lsp_call_edge_count,
                            existing_coverage,
                            EnrichmentScope::ChangedFiles.stable_key(),
                        );
                    }
                    if lsp_edge_count > 0 {
                        tracing::info!(
                            "[background-lsp] LSP enrichment complete: {} LSP call edges, {} total LSP edges in {:.2}s",
                            lsp_call_edge_count,
                            lsp_edge_count,
                            t0.elapsed().as_secs_f64()
                        );
                    } else {
                        tracing::info!(
                            "[background-lsp] LSP enrichment completed with no edges in {:.2}s",
                            t0.elapsed().as_secs_f64()
                        );
                    }

                    // Dedup
                    {
                        let mut seen_nodes = std::collections::HashSet::new();
                        enriched_nodes.reverse();
                        enriched_nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                        enriched_nodes.reverse();
                        let mut seen_edges = std::collections::HashSet::new();
                        enriched_edges.retain(|e| seen_edges.insert(e.stable_id()));
                    }

                    // Rebuild petgraph index
                    let mut index = crate::graph::index::GraphIndex::new();
                    index.rebuild_from_edges(&enriched_edges);
                    for node in &enriched_nodes {
                        index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                    }

                    // Recompute PageRank with LSP edges
                    let pagerank_scores = index.compute_pagerank(0.85, 20);
                    for node in &mut enriched_nodes {
                        if let Some(&score) = pagerank_scores.get(&node.stable_id()) {
                            node.metadata
                                .insert("importance".to_string(), format!("{:.6}", score));
                        }
                    }

                    // Re-run subsystem detection with updated PageRank
                    {
                        let node_file_map: std::collections::HashMap<String, String> =
                            enriched_nodes
                                .iter()
                                .filter(|n| n.id.root != "external")
                                .map(|n| (n.stable_id(), n.id.file.display().to_string()))
                                .collect();
                        let mut subsystems =
                            index.detect_communities(&pagerank_scores, &node_file_map);
                        // Dedup subsystem names
                        {
                            let mut name_counts: std::collections::HashMap<String, usize> =
                                std::collections::HashMap::new();
                            for s in &subsystems {
                                *name_counts.entry(s.name.clone()).or_default() += 1;
                            }
                            for s in &mut subsystems {
                                if name_counts.get(&s.name).copied().unwrap_or(0) > 1
                                    && let Some(iface) = s.interfaces.first()
                                {
                                    let short = iface
                                        .node_id
                                        .split(':')
                                        .rev()
                                        .nth(1)
                                        .unwrap_or(&iface.node_id);
                                    s.name = format!("{}/{}", s.name, short);
                                }
                            }
                        }
                        let mut node_subsystem: std::collections::HashMap<String, String> =
                            std::collections::HashMap::new();
                        for subsystem in &subsystems {
                            for member_id in &subsystem.member_ids {
                                node_subsystem.insert(member_id.clone(), subsystem.name.clone());
                            }
                        }
                        // Remove stale virtual nodes
                        enriched_nodes.retain(|n| !matches!(&n.id.kind, crate::graph::NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event")));
                        enriched_edges.retain(|e| {
                            !matches!(&e.to.kind, crate::graph::NodeKind::Other(s) if s == "subsystem")
                                && e.kind != crate::graph::EdgeKind::UsesFramework
                                && e.kind != crate::graph::EdgeKind::Produces
                                && e.kind != crate::graph::EdgeKind::Consumes
                        });
                        for node in &mut enriched_nodes {
                            if let Some(subsystem_name) = node_subsystem.get(&node.stable_id()) {
                                node.metadata.insert(
                                    super::graph::SUBSYSTEM_KEY.to_owned(),
                                    subsystem_name.clone(),
                                );
                            } else {
                                node.metadata.remove(super::graph::SUBSYSTEM_KEY);
                            }
                        }
                        // Emit subsystem virtual nodes
                        let (sub_added_nodes, sub_added_edges) =
                            crate::extract::consumers::emit_community_detection(
                                primary_slug,
                                subsystems,
                                enriched_nodes.clone(),
                            )
                            .await
                            .unwrap_or_else(|e| {
                                tracing::warn!(
                                    "[background-lsp] Subsystem promotion failed: {}",
                                    e
                                );
                                (vec![], vec![])
                            });
                        enriched_nodes.extend(sub_added_nodes);
                        enriched_edges.extend(sub_added_edges);

                        // Re-add virtual nodes to index
                        for node in &enriched_nodes {
                            if matches!(&node.id.kind, crate::graph::NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event"))
                            {
                                index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                            }
                        }
                        for edge in &enriched_edges {
                            if matches!(&edge.to.kind, crate::graph::NodeKind::Other(s) if matches!(s.as_str(), "subsystem" | "framework" | "channel" | "event"))
                            {
                                index.add_edge(
                                    &edge.from.to_stable_id(),
                                    &edge.from.kind.to_string(),
                                    &edge.to.to_stable_id(),
                                    &edge.to.kind.to_string(),
                                    edge.kind.clone(),
                                );
                            }
                        }
                    }

                    // Final dedup
                    {
                        let mut seen_edges = std::collections::HashSet::new();
                        enriched_edges.retain(|e| seen_edges.insert(e.stable_id()));
                    }

                    jobs.mark_persisting(
                        &repo_root,
                        &job_id,
                        enriched_nodes.len(),
                        enriched_edges.len(),
                    );
                    // Persist to LanceDB with LSP edges
                    {
                        let _lance_guard = lance_write_lock.lock().await;
                        if let Err(e) = super::store::persist_graph_to_lance(
                            &repo_root,
                            &enriched_nodes,
                            &enriched_edges,
                        )
                        .await
                        {
                            tracing::error!("[background-lsp] LanceDB persist failed: {}", e);
                            let detail = format!("persistence failed: {e}");
                            jobs.mark_failed(&repo_root, &job_id, detail.clone());
                            jobs.record_lsp_evidence(
                                &repo_root,
                                &job_id,
                                lsp_evidence(
                                    LspEvidenceReadiness::Partial,
                                    &EnrichmentScope::ChangedFiles.stable_key(),
                                    0,
                                    None,
                                    Some(detail.clone()),
                                    validations.clone(),
                                ),
                            );
                        } else {
                            super::sentinel::write_extract_sentinel(
                                &repo_root,
                                enriched_nodes.len(),
                                enriched_edges.len(),
                            );
                            let has_lsp_edges = enriched_edges
                                .iter()
                                .any(|e| e.source == crate::graph::ExtractionSource::Lsp);
                            if has_lsp_edges && diagnostics.is_empty() {
                                super::sentinel::write_lsp_sentinel(
                                    &repo_root,
                                    enriched_nodes.len(),
                                    enriched_edges.len(),
                                );
                            } else if !diagnostics.is_empty() {
                                super::sentinel::clear_lsp_sentinel(&repo_root);
                            }
                            tracing::info!(
                                "[background-lsp] LanceDB re-persisted with LSP edges: {} nodes, {} edges",
                                enriched_nodes.len(),
                                enriched_edges.len()
                            );
                            if let Some(detail) = degraded_detail.as_deref() {
                                jobs.mark_degraded(
                                    &repo_root,
                                    &job_id,
                                    enriched_nodes.len(),
                                    enriched_edges.len(),
                                    detail,
                                );
                            } else {
                                jobs.mark_completed(
                                    &repo_root,
                                    &job_id,
                                    enriched_nodes.len(),
                                    enriched_edges.len(),
                                );
                            }
                            jobs.record_lsp_evidence(
                                &repo_root,
                                &job_id,
                                lsp_evidence(
                                    if degraded_detail.is_some() {
                                        LspEvidenceReadiness::Partial
                                    } else {
                                        LspEvidenceReadiness::Scoped
                                    },
                                    &EnrichmentScope::ChangedFiles.stable_key(),
                                    0,
                                    None,
                                    degraded_detail.clone(),
                                    validations.clone(),
                                ),
                            );
                        }
                    }

                    // ArcSwap the fully enriched graph
                    let all_frameworks = detected_frameworks
                        .union(&enriched_frameworks)
                        .cloned()
                        .collect();
                    let new_state = super::state::GraphState::new(
                        enriched_nodes,
                        enriched_edges,
                        index,
                        Some(std::time::Instant::now()),
                        all_frameworks,
                    );
                    graph_arc.store(Arc::new(Some(Arc::new(new_state))));
                    tracing::info!(
                        "[background-lsp] Enriched graph swapped in after {:.2}s",
                        t0.elapsed().as_secs_f64()
                    );
                }
                Err(e) => {
                    tracing::error!("[background-lsp] LSP enrichment pipeline failed: {:#}", e);
                    if e.is_timeout() {
                        lsp_status.set_timed_out(&format!("{}", e));
                        jobs.mark_timed_out(&repo_root, &job_id, format!("{}", e));
                    } else {
                        lsp_status.set_unavailable();
                        jobs.mark_failed(&repo_root, &job_id, format!("{}", e));
                    }
                }
            }
        })
    }

    /// Spawn background embedding after a full graph build.
    ///
    /// **Phase 3**: LSP enrichment has been moved into `LspConsumer` within the event bus
    /// (via `emit_enrichment_pipeline`). This function now handles only the embedding pipeline.
    ///
    /// The graph is queryable NOW -- embedding improves semantic search quality progressively.
    pub(crate) fn spawn_background_enrichment(
        &self,
        all_nodes: &[Node],
    ) -> tokio::task::JoinHandle<()> {
        let bg_repo_root = self.repo_root.clone();
        let bg_embed_index = self.embed_index.clone();
        let bg_embed_status = self.embed_status.clone();
        let bg_business_context = self.business_context.clone();
        let bg_nodes = all_nodes.to_vec();
        let job_id = match self.enrichment_jobs.begin_job(
            &self.repo_root,
            EnrichmentCapability::Embeddings,
            EnrichmentScope::Repo,
            EnrichmentTrigger::BackgroundScan,
            None,
        ) {
            Ok(JobStart::Started(job)) => job.job_id,
            Ok(JobStart::Joined { existing_job_id }) => {
                tracing::info!(
                    "[background] joining active embedding job {}",
                    existing_job_id
                );
                return tokio::spawn(async {});
            }
            Err(e) => {
                tracing::warn!("[background] failed to begin embedding job: {}", e);
                return tokio::spawn(async {});
            }
        };
        let bg_jobs = Arc::clone(&self.enrichment_jobs);

        tokio::spawn(async move {
            bg_jobs.mark_running(&bg_repo_root, &job_id, "embedding");
            let embeddable_nodes: Vec<Node> = bg_nodes
                .iter()
                .filter(|n| n.id.root != "external")
                .cloned()
                .collect();

            let embed_repo_root = bg_repo_root.clone();
            let embed_index_ref = bg_embed_index.clone();
            let embed_status = bg_embed_status;
            let embeddable_count = embeddable_nodes
                .iter()
                .filter(|n| n.id.kind.is_embeddable())
                .count();
            embed_status.set_building(embeddable_count);
            bg_jobs.mark_progress(
                &bg_repo_root,
                &job_id,
                "embedding",
                0,
                Some(embeddable_count),
            );

            let embed_fut = async move {
                // Use BLAKE3 incremental reindex: hash-skip unchanged items
                // instead of dropping and rebuilding the entire table.
                // Falls back to full rebuild only if the table doesn't exist yet.
                match EmbeddingIndex::new(&embed_repo_root).await {
                    Ok(idx) => {
                        let result = match idx.has_table().await {
                            Ok(true) => {
                                // Table exists -- use incremental reindex with BLAKE3 hash-skipping
                                idx.reindex_nodes(&embeddable_nodes).await
                            }
                            Ok(false) => {
                                // Table missing -- full build needed
                                idx.index_all_with_symbols_and_business_context(
                                    &embed_repo_root,
                                    &embeddable_nodes,
                                    &bg_business_context,
                                )
                                .await
                            }
                            Err(e) => {
                                tracing::warn!("[background] Embedding table check failed: {}", e);
                                embed_status.set_failed(format!("{}", e));
                                return;
                            }
                        };
                        match result {
                            Ok(count) => {
                                tracing::info!("[background] Embedded {} items", count);
                                embed_status.set_complete(count);
                                // Atomic store -- no mutex needed
                                embed_index_ref.store(Arc::new(Some(idx)));
                                bg_jobs.mark_completed(&bg_repo_root, &job_id, count, count);
                            }
                            Err(e) => {
                                tracing::warn!("[background] Embedding failed: {}", e);
                                embed_status.set_failed(format!("{}", e));
                                bg_jobs.mark_failed(&bg_repo_root, &job_id, format!("{}", e));
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("[background] EmbeddingIndex init failed: {}", e);
                        embed_status.set_failed(format!("{}", e));
                        bg_jobs.mark_failed(&bg_repo_root, &job_id, format!("{}", e));
                    }
                }
            };

            // Phase 3: LSP enrichment now runs inside the event bus via LspConsumer.
            // This function is embedding-only; no lsp_fut here.
            embed_fut.await;
        })
    }

    /// Spawn background LSP enrichment for the cache-hit path, routing through the event bus.
    ///
    /// Called from `build_full_graph_inner` when no files changed and the graph was loaded from
    /// LanceDB but the LSP sentinel is absent (LSP did not complete in the previous run).
    ///
    /// Unlike `spawn_lsp_enrichment`, this path routes through `LspConsumer` → `EnrichmentComplete`
    /// → `AllEnrichmentsGate` → `AllEnrichmentsDone` → `EnrichmentFinalizer` → `PassesComplete`,
    /// so `ScanStatsConsumer` correctly tracks LSP completion and no bus consumers are bypassed.
    ///
    /// The full enrichment pipeline is called with the cached nodes. The resulting enriched
    /// nodes/edges replace the in-memory graph state, and the LSP sentinel is written so
    /// subsequent restarts skip re-enrichment.
    pub(crate) fn spawn_lsp_enrichment_via_bus(&self, nodes: &[Node], edges: &[Edge]) {
        let bg_repo_root = self.repo_root.clone();
        let bg_graph = self.graph.clone();
        let bg_lsp_status = self.lsp_status.clone();
        let bg_lance_write_lock = Arc::clone(&self.lance_write_lock);
        let bg_scan_stats = Arc::clone(&self.scan_stats);
        let bg_nodes: Vec<Node> = nodes.to_vec();
        let bg_edges: Vec<Edge> = edges.to_vec();
        let job_id = match self.enrichment_jobs.begin_job(
            &self.repo_root,
            EnrichmentCapability::CallReferences,
            EnrichmentScope::Repo,
            EnrichmentTrigger::Startup,
            None,
        ) {
            Ok(JobStart::Started(job)) => job.job_id,
            Ok(JobStart::Joined { existing_job_id }) => {
                tracing::info!("[cache-hit bus] joining active LSP job {}", existing_job_id);
                return;
            }
            Err(e) => {
                tracing::warn!("[cache-hit bus] failed to begin LSP job: {}", e);
                return;
            }
        };
        let bg_jobs = Arc::clone(&self.enrichment_jobs);

        bg_lsp_status.set_running();

        tokio::spawn(async move {
            bg_jobs.mark_running(&bg_repo_root, &job_id, "lsp");
            tracing::info!(
                "[cache-hit bus] LSP enrichment via bus starting with {} nodes, {} edges",
                bg_nodes.len(),
                bg_edges.len()
            );

            // Build workspace root_pairs needed by emit_enrichment_pipeline / LspConsumer.
            let workspace = WorkspaceConfig::load()
                .with_primary_root(bg_repo_root.clone())
                .with_worktrees(&bg_repo_root)
                .with_claude_memory(&bg_repo_root)
                .with_agent_memories(&bg_repo_root)
                .with_declared_roots(&bg_repo_root);
            let root_pairs: Vec<(String, std::path::PathBuf)> = workspace
                .resolved_roots()
                .iter()
                .map(|r| (r.slug.clone(), r.path.clone()))
                .collect();
            let primary_slug = RootConfig::code_project(bg_repo_root.clone()).slug();

            // Run the full enrichment pipeline (bus path): LanguageDetected → LspConsumer
            // → EnrichmentComplete → AllEnrichmentsGate → AllEnrichmentsDone → EnrichmentFinalizer
            // → PassesComplete. scan_stats is wired in so ScanStatsConsumer tracks LSP completion.
            //
            // Consume bg_nodes/bg_edges via move (no redundant clone; these are the only owners).
            // LanceDB persist is handled below after replacing the in-memory graph.
            let bus_repo_root = bg_repo_root.clone();
            // Cache-hit LSP enrichment: all roots are "dirty" because this is
            // the first time LSP runs on a cached graph (no prior LSP edges).
            // `None` = all roots dirty on first LSP run.
            let result = emit_lsp_pipeline_with_budget(LspPipelineInput {
                nodes: bg_nodes,
                edges: bg_edges,
                root_pairs,
                primary_slug,
                repo_root: bus_repo_root,
                scan_stats: bg_scan_stats,
                skip_lsp: false,
                dirty_slugs: None,
                lsp_node_filter: None,
                file_readiness_filter: None,
                broad_reference_budget: None,
            })
            .await;

            let (
                mut enriched_nodes,
                mut enriched_edges,
                _detected_frameworks,
                diagnostics,
                validations,
            ) = match result {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("[cache-hit bus] emit_enrichment_pipeline failed: {:#}", e);
                    if e.is_timeout() {
                        bg_lsp_status.set_timed_out(&format!("{}", e));
                        bg_jobs.mark_timed_out(&bg_repo_root, &job_id, format!("{}", e));
                    } else {
                        bg_lsp_status.set_failed(&format!("{}", e));
                        bg_jobs.mark_failed(&bg_repo_root, &job_id, format!("{}", e));
                    }
                    return;
                }
            };

            // Dedup: PassesComplete can re-emit cached entries when the cached graph already
            // contains output from a previous pass run. Dedup avoids duplicate rows in LanceDB
            // and inflated edge weights (same logic as the full-build path in graph.rs).
            {
                let mut seen_nodes = std::collections::HashSet::new();
                enriched_nodes.reverse();
                enriched_nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                enriched_nodes.reverse();

                let mut seen_edges = std::collections::HashSet::new();
                enriched_edges.retain(|e| seen_edges.insert(e.stable_id()));
            }

            // Count LSP-sourced edges to determine enrichment status.
            let lsp_call_edge_count = enriched_edges
                .iter()
                .filter(|e| {
                    e.source == crate::graph::ExtractionSource::Lsp
                        && matches!(e.kind, crate::graph::EdgeKind::Calls)
                })
                .count();
            let lsp_edge_count = enriched_edges
                .iter()
                .filter(|e| e.source == crate::graph::ExtractionSource::Lsp)
                .count();
            bg_jobs.mark_progress(
                &bg_repo_root,
                &job_id,
                "lsp_edges",
                lsp_call_edge_count,
                Some(lsp_edge_count),
            );

            tracing::info!(
                "[cache-hit bus] LSP enrichment complete: {} LSP call edges, {} total LSP edges, {} total nodes",
                lsp_call_edge_count,
                lsp_edge_count,
                enriched_nodes.len()
            );

            // Build updated index from enriched edges.
            let mut new_index = crate::graph::index::GraphIndex::new();
            new_index.rebuild_from_edges(&enriched_edges);
            for node in &enriched_nodes {
                new_index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
            }

            // Atomic swap: replace the in-memory graph with the enriched version.
            // Tool calls reading the old snapshot are undisturbed; new calls see
            // the enriched version immediately.
            {
                let snap = bg_graph.load_full();
                if let Some(ref gs) = *snap {
                    let mut new_gs = (**gs).clone();
                    new_gs.nodes = enriched_nodes.clone();
                    new_gs.edges = enriched_edges.clone();
                    new_gs.index = new_index;
                    bg_graph.store(Arc::new(Some(Arc::new(new_gs))));
                }
            }

            bg_jobs.mark_persisting(
                &bg_repo_root,
                &job_id,
                enriched_nodes.len(),
                enriched_edges.len(),
            );
            // Persist enriched graph to LanceDB and write sentinel under the write lock
            // so no concurrent writer can interleave between persist and sentinel write.
            let persist_result = {
                let _lance_guard = bg_lance_write_lock.lock().await;
                let result =
                    persist_graph_to_lance(&bg_repo_root, &enriched_nodes, &enriched_edges).await;
                if result.is_ok() {
                    if diagnostics.is_empty() {
                        super::sentinel::write_lsp_sentinel(
                            &bg_repo_root,
                            enriched_nodes.len(),
                            enriched_edges.len(),
                        );
                    } else {
                        super::sentinel::clear_lsp_sentinel(&bg_repo_root);
                    }
                }
                result
            };

            match persist_result {
                Ok(()) => {
                    tracing::info!(
                        "[cache-hit bus] LSP persist complete: {} nodes, {} edges",
                        enriched_nodes.len(),
                        enriched_edges.len()
                    );
                    // Mirror other LSP paths: set_complete(0) when no edges (enricher ran but found
                    // nothing), not set_unavailable(). The sentinel is written to prevent repeated
                    // re-enrichment on repos that legitimately produce zero LSP edges.
                    let degraded_detail = (!diagnostics.is_empty()).then(|| diagnostics.join("; "));
                    if let Some(detail) = degraded_detail.as_deref() {
                        bg_lsp_status.set_degraded(lsp_call_edge_count, detail);
                    } else {
                        bg_lsp_status.set_complete_default_profile_for_warmup(lsp_call_edge_count);
                    }
                    if let Some(detail) = degraded_detail.as_deref() {
                        bg_jobs.mark_degraded(
                            &bg_repo_root,
                            &job_id,
                            enriched_nodes.len(),
                            enriched_edges.len(),
                            detail,
                        );
                    } else {
                        bg_jobs.mark_completed(
                            &bg_repo_root,
                            &job_id,
                            enriched_nodes.len(),
                            enriched_edges.len(),
                        );
                    }
                    bg_jobs.record_lsp_evidence(
                        &bg_repo_root,
                        &job_id,
                        lsp_evidence(
                            if degraded_detail.is_some() {
                                LspEvidenceReadiness::Partial
                            } else {
                                LspEvidenceReadiness::DefaultProfile
                            },
                            &EnrichmentScope::Repo.stable_key(),
                            0,
                            None,
                            degraded_detail.clone().or_else(|| {
                                Some(
                                    "repo-wide default query profile completed; broad references were omitted"
                                        .to_string(),
                                )
                            }),
                            validations.clone(),
                        ),
                    );
                }
                Err(e) => {
                    tracing::error!("[cache-hit bus] LSP persist failed: {:#}", e);
                    bg_lsp_status.set_complete_persist_failed(lsp_call_edge_count);
                    let detail = format!("persistence failed: {e}");
                    bg_jobs.mark_failed(&bg_repo_root, &job_id, detail.clone());
                    bg_jobs.record_lsp_evidence(
                        &bg_repo_root,
                        &job_id,
                        lsp_evidence(
                            LspEvidenceReadiness::Partial,
                            &EnrichmentScope::Repo.stable_key(),
                            0,
                            None,
                            Some(detail.clone()),
                            validations,
                        ),
                    );
                }
            }
        });
    }

    /// Build workspace `root_pairs` and `primary_slug` for `emit_enrichment_pipeline`.
    ///
    /// Factored out to avoid duplicating `WorkspaceConfig` boilerplate across
    /// every foreground path that now routes through the event bus (ADR-001, #583).
    fn build_bus_root_pairs(&self) -> (Vec<(String, PathBuf)>, String) {
        let workspace = WorkspaceConfig::load()
            .with_primary_root(self.repo_root.clone())
            .with_worktrees(&self.repo_root)
            .with_claude_memory(&self.repo_root)
            .with_agent_memories(&self.repo_root)
            .with_declared_roots(&self.repo_root);
        let root_pairs: Vec<(String, PathBuf)> = workspace
            .resolved_roots()
            .iter()
            .map(|r| (r.slug.clone(), r.path.clone()))
            .collect();
        let primary_slug = RootConfig::code_project(self.repo_root.clone()).slug();
        (root_pairs, primary_slug)
    }

    /// Run the full pipeline synchronously with progress reporting.
    ///
    /// This is the `--full` CLI path. When a cached graph exists in LanceDB,
    /// it uses the incremental path (only re-extract changed files, LSP on
    /// changed nodes only) for dramatically faster rescans. Falls back to
    /// full rebuild when no cache exists.
    ///
    /// The `on_progress` callback receives structured status messages.
    pub async fn run_pipeline_foreground<F>(
        &self,
        on_progress: F,
        enrichment: ScanEnrichmentOptions,
    ) -> anyhow::Result<PipelineResult>
    where
        F: Fn(&str) + Send + Sync,
    {
        let pipeline_start = std::time::Instant::now();

        // The foreground pipeline has its own Lance cache-load path instead of
        // delegating to `build_full_graph_inner`. Validate the requested mode
        // before even checking that cache so a legacy or mismatched index is
        // deleted and rebuilt rather than entering the incremental path.
        self.prepare_business_context_cache()?;

        // Try incremental path: load cached graph, apply delta, LSP on changed nodes.
        let lance_path = super::store::graph_lance_path(&self.repo_root);
        let cached = if lance_path.exists() {
            match super::store::load_graph_from_lance(&self.repo_root).await {
                Ok(state) => {
                    on_progress(&format!(
                        "Loaded cached graph: {} nodes, {} edges",
                        state.nodes.len(),
                        state.edges.len(),
                    ));
                    Some(state)
                }
                Err(e) => {
                    tracing::debug!(
                        "Could not load cached graph, falling back to full rebuild: {}",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        if let Some(cached_state) = cached {
            return self
                .run_pipeline_foreground_incremental(
                    cached_state,
                    on_progress,
                    pipeline_start,
                    enrichment,
                )
                .await;
        }

        // No cache -- full rebuild path.
        self.run_pipeline_foreground_full(on_progress, pipeline_start, enrichment)
            .await
    }

    /// Incremental foreground pipeline: load from cache, extract only changed files,
    /// LSP enrich only changed nodes, re-embed only changed symbols.
    async fn run_pipeline_foreground_incremental<F>(
        &self,
        mut cached_state: GraphState,
        on_progress: F,
        pipeline_start: std::time::Instant,
        enrichment: ScanEnrichmentOptions,
    ) -> anyhow::Result<PipelineResult>
    where
        F: Fn(&str) + Send + Sync,
    {
        // Pre-flight: ensure schema version matches. If migration happened,
        // the cache was rebuilt and our loaded graph is stale -- fall back to
        // full rebuild by returning an error that the caller can catch.
        let db_path = super::store::graph_lance_path(&self.repo_root);
        if super::store::check_and_migrate_schema(&db_path).await? {
            tracing::info!(
                "Schema migrated during incremental pre-flight -- falling back to full rebuild"
            );
            on_progress("Schema migration detected -- rebuilding from scratch.");
            // Clear sentinels -- they reference the old schema version and are now stale.
            super::sentinel::clear_sentinels(&self.repo_root);
            return self
                .run_pipeline_foreground_full(on_progress, pipeline_start, enrichment)
                .await;
        }

        // Phase 1: Scan to detect changes.
        let t0 = std::time::Instant::now();
        let mut scanner = Scanner::new(self.repo_root.clone())?;
        let scan = scanner.scan()?;
        let scan_time = t0.elapsed();

        let change_count =
            scan.changed_files.len() + scan.new_files.len() + scan.deleted_files.len();
        let cache_authorization =
            crate::structural_cache::load_verified_authorization(&self.repo_root)?;
        let preliminary_cache_plan = cache_authorization.as_ref().map(|authorization| {
            crate::structural_cache::plan_incremental_impact(
                authorization,
                &cached_state.nodes,
                &cached_state.edges,
                &cached_state.nodes,
                &cached_state.edges,
            )
        });

        on_progress(&format!(
            "Scan: {} changed, {} new, {} deleted in {:.1}s",
            scan.changed_files.len(),
            scan.new_files.len(),
            scan.deleted_files.len(),
            scan_time.as_secs_f64(),
        ));

        if change_count == 0
            && preliminary_cache_plan
                .as_ref()
                .is_none_or(|plan| plan.executed_paths.is_empty())
        {
            // FIX(#601): check if the cached graph is missing enrichment output
            // (e.g., framework nodes). Caches built before the event bus was wired
            // (pre-v2-rc) or by a binary that skipped post-extraction passes may lack
            // framework nodes even though Import nodes are present. When stale, clear
            // sentinels and re-run the full enrichment pipeline.
            let stale_enrichment = cache_needs_enrichment(&cached_state.nodes);
            if stale_enrichment {
                on_progress(
                    "No file changes but cached graph missing enrichment output -- re-enriching...",
                );
                super::sentinel::clear_sentinels(&self.repo_root);
            } else {
                on_progress("No changes detected -- reusing cached graph.");
            }

            // Store graph atomically and set up embedding index.
            self.graph.store(Arc::new(Some(Arc::new(cached_state))));

            // Reuse existing embedding index.
            if let Ok(idx) = EmbeddingIndex::new(&self.repo_root).await
                && let Ok(true) = idx.has_table().await
            {
                idx.ensure_fts_index().await;
                self.embed_index.store(Arc::new(Some(idx)));
            }

            // Check if LSP enrichment has been durably persisted via the completion
            // sentinel. This replaces the `has_call_edges` heuristic, which fails
            // when LSP ran but the subsequent LanceDB persist crashed: edges end up
            // in memory but the sentinel is never written, so the next restart
            // correctly re-runs LSP enrichment (#477).
            let lsp_sentinel = super::sentinel::read_lsp_sentinel(&self.repo_root);

            let (lsp_edge_count, lsp_job_id) = if lsp_sentinel.is_some() && !stale_enrichment {
                let call_count = {
                    let snap = self.graph.load_full();
                    snap.as_ref()
                        .as_ref()
                        .unwrap()
                        .edges
                        .iter()
                        .filter(|e| matches!(e.kind, crate::graph::EdgeKind::Calls))
                        .count()
                };
                self.lsp_status
                    .set_complete_default_profile_for_warmup(call_count);
                on_progress(&format!(
                    "LSP: {} cached call edges (sentinel present)",
                    call_count
                ));
                (call_count, None)
            } else if enrichment.runs_lsp() {
                // LSP sentinel absent or stale enrichment -- run full enrichment.
                on_progress("LSP: running full enrichment...");
                let run = self
                    .run_foreground_lsp_and_persist(
                        &on_progress,
                        ForegroundLspRequest {
                            scope: EnrichmentScope::Repo,
                            trigger: EnrichmentTrigger::ForegroundScan,
                            dirty_slugs: None,
                            node_filter: None,
                            fail_on_lsp_error: true,
                            broad_reference_budget: None,
                            declared_node_count: 0,
                        },
                    )
                    .await?;
                (run.edge_count, Some(run.job_id))
            } else {
                on_progress("LSP: skipped by scan options");
                (0, None)
            };

            scanner.commit_state()?;

            if let (Some(authorization), Some(plan)) = (
                cache_authorization.as_ref(),
                preliminary_cache_plan.as_ref(),
            ) {
                crate::structural_cache::write_execution(
                    &self.repo_root,
                    &crate::structural_cache::StructuralCacheExecution {
                        schema_version:
                            crate::structural_cache::STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION,
                        offline_preprocessing: true,
                        base_archive_sha256: authorization
                            .authorization
                            .base_archive_sha256
                            .clone(),
                        base_sidecar_sha256: authorization
                            .authorization
                            .base_sidecar_sha256
                            .clone(),
                        base_report_digest: authorization.authorization.base_report_digest.clone(),
                        target_commit: authorization.authorization.target_commit.clone(),
                        target_tree: authorization.authorization.target_tree.clone(),
                        inherited_paths: plan
                            .inherited_paths
                            .iter()
                            .map(|path| path.to_string_lossy().to_string())
                            .collect(),
                        executed_paths: Vec::new(),
                        invalidated_partitions: authorization
                            .authorization
                            .invalidated_partitions
                            .clone(),
                        escalated_partitions: Vec::new(),
                        changed_file_count: 0,
                        invalidated_file_count: 0,
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
                        executed_graph_enrichment_operation_count: 0,
                        inherited_readiness_validation_request_count: authorization
                            .inherited_readiness_validation_request_count(&plan.inherited_paths),
                        executed_readiness_validation_request_count: 0,
                        executed_producer_work_ids: Vec::new(),
                        closure_edge_count: plan.closure_edge_count as u64,
                        execution_job_id: None,
                        digest: String::new(),
                    },
                )?;
            }

            // Read final counts (may have changed after LSP enrichment).
            let (total_node_count, total_edge_count) = {
                let snap = self.graph.load_full();
                let gs = snap.as_ref().as_ref().unwrap();
                (gs.nodes.len(), gs.edges.len())
            };

            let total_time = pipeline_start.elapsed();
            on_progress(&format!(
                "Graph: {} nodes, {} edges",
                total_node_count, total_edge_count
            ));
            on_progress(&format!(
                "Done in {:.1}s (incremental, no changes)",
                total_time.as_secs_f64()
            ));

            let mut phases = vec![
                PhaseReport::ran(PhaseKind::DiscoverFiles, scan_time),
                PhaseReport::ran(PhaseKind::Total, total_time),
            ];
            if !enrichment.runs_lsp() {
                phases.push(PhaseReport::skipped(
                    PhaseKind::Lsp,
                    "skipped by scan options",
                ));
            }
            if !enrichment.runs_embeddings() {
                phases.push(PhaseReport::skipped(
                    PhaseKind::Embeddings,
                    "skipped by scan options",
                ));
            }
            let related_job_ids = lsp_job_id.into_iter().collect::<Vec<_>>();
            let (lsp_state, lsp_detail) = lsp_capability_from_status(
                enrichment,
                self.lsp_status.current_state(),
                self.lsp_status.diagnostic().as_deref(),
                lsp_edge_count,
                !related_job_ids.is_empty(),
            );
            if matches!(lsp_state, CapabilityState::Failed) {
                phases.push(PhaseReport::failed(
                    PhaseKind::Lsp,
                    total_time,
                    lsp_detail
                        .clone()
                        .unwrap_or_else(|| "call-reference enrichment failed".to_string()),
                ));
            } else if matches!(lsp_state, CapabilityState::Unavailable) {
                phases.push(PhaseReport::unavailable(
                    PhaseKind::Lsp,
                    lsp_detail
                        .clone()
                        .unwrap_or_else(|| "call-reference enrichment unavailable".to_string()),
                ));
            }
            let report = build_pipeline_operation_report(
                &self.repo_root,
                PipelineReportInput {
                    operation: OperationKind::CacheLoad,
                    enrichment,
                    duration: total_time,
                    symbol_count: total_node_count,
                    edge_count: total_edge_count,
                    file_count: 0,
                    lsp_edge_count,
                    lsp_state,
                    lsp_detail,
                    embedding_count: 0,
                    embeddings_attached: self.embed_index.load().is_some(),
                    phases,
                    related_job_ids,
                    business_context: self.business_context.clone(),
                },
            );

            return Ok(PipelineResult {
                node_count: total_node_count,
                edge_count: total_edge_count,
                file_count: 0,
                lsp_edge_count,
                embed_count: 0,
                total_time,
                lsp_entries: vec![],
                encoding_stats: crate::extract::EncodingStats::default(),
                report,
            });
        }

        // Phase 2: Incremental extract -- only changed files.
        let t1 = std::time::Instant::now();

        // Track changed file paths for LSP scoping.
        let changed_file_set: std::collections::HashSet<PathBuf> = scan
            .changed_files
            .iter()
            .chain(scan.new_files.iter())
            .cloned()
            .collect();
        let old_nodes = cached_state.nodes.clone();
        let old_edges = cached_state.edges.clone();

        // Rebuild the index before update_graph_with_scan (it expects a valid index).
        cached_state.index = crate::graph::index::GraphIndex::new();
        cached_state.index.rebuild_from_edges(&cached_state.edges);
        for node in &cached_state.nodes {
            cached_state
                .index
                .ensure_node(&node.stable_id(), &node.id.kind.to_string());
        }

        // The outer foreground bus below is the sole LSP owner for an
        // incremental target. The graph update still performs extraction and
        // structural post-passes, but cannot execute the same LSP plan twice.
        let _incremental_update = self
            .update_graph_with_scan_outcome(&mut cached_state, Some(scan), enrichment.without_lsp())
            .await?;
        let cache_plan = cache_authorization.as_ref().map(|authorization| {
            crate::structural_cache::plan_incremental_impact(
                authorization,
                &old_nodes,
                &old_edges,
                &cached_state.nodes,
                &cached_state.edges,
            )
        });
        self.graph.store(Arc::new(Some(Arc::new(cached_state))));

        let extract_time = t1.elapsed();

        let (node_count, file_count) = {
            let snap = self.graph.load_full();
            let gs = snap.as_ref().as_ref().unwrap();
            let files: std::collections::HashSet<_> = gs
                .nodes
                .iter()
                .map(|n| n.id.file.to_string_lossy().to_string())
                .collect();
            (gs.nodes.len(), files.len())
        };

        on_progress(&format!(
            "Incremental extract: {} symbols across {} files in {:.1}s (only {} files re-extracted)",
            node_count,
            file_count,
            extract_time.as_secs_f64(),
            changed_file_set.len(),
        ));

        // Phase 3: Enrichment via event bus (ADR-001, #583).
        // Route through emit_enrichment_pipeline instead of calling
        // EnricherRegistry::enrich_all() directly. Pass the full graph;
        // dirty_slugs scopes LSP to only the primary root (changed files).
        let (mut all_nodes, mut all_edges) = {
            let snap = self.graph.load_full();
            let gs = snap.as_ref().as_ref().unwrap();
            (gs.nodes.clone(), gs.edges.clone())
        };

        if enrichment.runs_lsp() {
            let server_name = self.lsp_status.server_name();
            if let Some(ref name) = server_name {
                on_progress(&format!("LSP: {} found on PATH", name));
            }
            self.lsp_status.set_running();
        }

        on_progress(&format!(
            "Enrichment: running pipeline via event bus ({} changed files)...",
            changed_file_set.len(),
        ));

        let (root_pairs, primary_slug) = self.build_bus_root_pairs();
        let lsp_execution_files = cache_plan
            .as_ref()
            .map(|plan| plan.executed_paths.iter().cloned().collect::<HashSet<_>>())
            .unwrap_or_else(|| changed_file_set.clone());
        if cache_plan.is_some() {
            let purge_paths = lsp_execution_files
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect::<BTreeSet<_>>();
            let purged_work = crate::extract::lsp::work_items::purge_records_for_paths(
                &self.repo_root,
                &purge_paths,
            )?;
            if !purged_work.is_empty() {
                on_progress(&format!(
                    "LSP: invalidated {} carried work records before incremental execution",
                    purged_work.len()
                ));
            }
        }
        let touched_files: std::collections::HashSet<(String, PathBuf)> = lsp_execution_files
            .iter()
            .cloned()
            .map(|file| (primary_slug.clone(), file))
            .collect();
        let empty_rebuilt_partitions = BTreeSet::new();
        let rebuilt_partitions = cache_plan
            .as_ref()
            .map(|plan| &plan.escalated_partitions)
            .unwrap_or(&empty_rebuilt_partitions);
        let lsp_node_filter =
            super::changed_file_plan::plan_lsp_node_ids_for_touched_files_with_partition_rebuilds(
                &touched_files,
                &all_nodes,
                rebuilt_partitions,
            )?;
        let purged_lsp_edge_ids = purge_existing_scoped_lsp_output(
            &mut all_nodes,
            &mut all_edges,
            &lsp_node_filter,
            &lsp_execution_files,
        );
        if !purged_lsp_edge_ids.is_empty() {
            on_progress(&format!(
                "LSP: invalidated {} cached scoped edges before incremental execution",
                purged_lsp_edge_ids.len()
            ));
        }
        // Only the primary root has changes in the incremental path.
        let dirty_slugs: Option<std::collections::HashSet<String>> =
            Some(std::iter::once(primary_slug.clone()).collect());
        let incremental_lsp_job_id = if enrichment.runs_lsp() && !lsp_execution_files.is_empty() {
            let job_id = match self.enrichment_jobs.begin_job(
                &self.repo_root,
                EnrichmentCapability::CallReferences,
                EnrichmentScope::ChangedFiles,
                EnrichmentTrigger::IncrementalRefresh,
                None,
            )? {
                JobStart::Started(job) => job.job_id,
                JobStart::Joined { existing_job_id } => existing_job_id,
            };
            self.enrichment_jobs.mark_running(
                &self.repo_root,
                &job_id,
                "incremental_call_references",
            );
            Some(job_id)
        } else {
            None
        };

        let mut lsp_stage_completed = false;
        let mut lsp_degraded_detail = None;
        let mut lsp_validations = Vec::new();
        let t2 = std::time::Instant::now();
        let bus_result = emit_lsp_pipeline_with_budget(LspPipelineInput {
            nodes: all_nodes,
            edges: all_edges,
            root_pairs,
            primary_slug,
            repo_root: self.repo_root.clone(),
            scan_stats: Arc::clone(&self.scan_stats),
            skip_lsp: !enrichment.runs_lsp() || incremental_lsp_job_id.is_none(),
            dirty_slugs,
            lsp_node_filter: Some(Arc::clone(&lsp_node_filter)),
            file_readiness_filter: Some(Arc::new(lsp_execution_files.clone())),
            broad_reference_budget: None,
        })
        .await;
        let bus_time = t2.elapsed();

        let lsp_edge_count;
        match bus_result {
            Ok((
                mut enriched_nodes,
                mut enriched_edges,
                detected_frameworks,
                diagnostics,
                bus_validations,
            )) => {
                lsp_validations = bus_validations;
                // Dedup: passes can re-emit cached entries.
                {
                    let mut seen_nodes = std::collections::HashSet::new();
                    enriched_nodes.reverse();
                    enriched_nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                    enriched_nodes.reverse();

                    dedup_edges_preserving_lsp_evidence(&mut enriched_edges);
                }

                lsp_edge_count = enriched_edges
                    .iter()
                    .filter(|e| {
                        e.source == crate::graph::ExtractionSource::Lsp
                            && matches!(e.kind, crate::graph::EdgeKind::Calls)
                            && (lsp_node_filter.contains(&e.from.to_stable_id())
                                || lsp_node_filter.contains(&e.to.to_stable_id()))
                    })
                    .count();

                on_progress(&format!(
                    "Enrichment: {} LSP call edges via bus in {:.1}s",
                    lsp_edge_count,
                    bus_time.as_secs_f64(),
                ));

                // Build updated index and apply enriched graph via atomic swap.
                let mut new_index = crate::graph::index::GraphIndex::new();
                new_index.rebuild_from_edges(&enriched_edges);
                for node in &enriched_nodes {
                    new_index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                }

                {
                    let snap = self.graph.load_full();
                    if let Some(ref current_gs) = *snap {
                        let mut gs = (**current_gs).clone();
                        gs.nodes = enriched_nodes;
                        gs.edges = enriched_edges;
                        gs.index = new_index;
                        gs.detected_frameworks = detected_frameworks;
                        self.graph.store(Arc::new(Some(Arc::new(gs))));
                    }
                }
                if enrichment.runs_lsp() {
                    let degraded_detail = (!diagnostics.is_empty()).then(|| diagnostics.join("; "));
                    if let Some(detail) = degraded_detail.as_deref() {
                        let existing_coverage = self.lsp_status.coverage_edge_count();
                        self.lsp_status.set_degraded_scoped(
                            lsp_edge_count,
                            existing_coverage,
                            EnrichmentScope::ChangedFiles.stable_key(),
                            detail,
                        );
                        lsp_degraded_detail = Some(detail.to_string());
                    } else {
                        let existing_coverage = self.lsp_status.coverage_edge_count();
                        self.lsp_status.set_complete_scoped(
                            lsp_edge_count,
                            existing_coverage,
                            EnrichmentScope::ChangedFiles.stable_key(),
                        );
                    }
                    lsp_stage_completed = true;
                }
            }
            Err(e) => {
                tracing::error!(
                    "Foreground incremental pipeline: emit_enrichment_pipeline failed: {:#}",
                    e
                );
                on_progress(&format!(
                    "Enrichment: pipeline failed in {:.1}s -- graph has tree-sitter data only",
                    bus_time.as_secs_f64(),
                ));
                if enrichment.runs_lsp() {
                    if e.is_timeout() {
                        self.lsp_status.set_timed_out(&format!("{}", e));
                    } else {
                        self.lsp_status.set_unavailable();
                    }
                    if let Some(job_id) = incremental_lsp_job_id.as_deref() {
                        if e.is_timeout() {
                            self.enrichment_jobs.mark_timed_out(
                                &self.repo_root,
                                job_id,
                                e.to_string(),
                            );
                        } else {
                            self.enrichment_jobs.mark_failed(
                                &self.repo_root,
                                job_id,
                                e.to_string(),
                            );
                        }
                        self.enrichment_jobs.record_lsp_evidence(
                            &self.repo_root,
                            job_id,
                            lsp_evidence(
                                LspEvidenceReadiness::Unavailable,
                                &EnrichmentScope::ChangedFiles.stable_key(),
                                0,
                                None,
                                Some(e.to_string()),
                                Vec::new(),
                            ),
                        );
                    }
                }
                lsp_edge_count = 0;
            }
        }

        // Phase 4: Full persist with LSP edges included.
        {
            let snapshot = {
                let snap = self.graph.load_full();
                snap.as_ref()
                    .as_ref()
                    .map(|gs| (gs.nodes.clone(), gs.edges.clone()))
            };
            if let Some((nodes, edges)) = snapshot {
                tracing::info!(
                    "Foreground incremental persist: {} nodes, {} edges (including {} LSP)",
                    nodes.len(),
                    edges.len(),
                    lsp_edge_count,
                );
                if let Err(e) = persist_graph_to_lance(&self.repo_root, &nodes, &edges).await {
                    tracing::error!("Foreground incremental persist failed: {}", e);
                    if lsp_stage_completed && let Some(job_id) = incremental_lsp_job_id.as_deref() {
                        let detail = format!(
                            "full persist failed during incremental foreground pipeline: {e}"
                        );
                        self.enrichment_jobs
                            .mark_failed(&self.repo_root, job_id, detail.clone());
                        self.enrichment_jobs.record_lsp_evidence(
                            &self.repo_root,
                            job_id,
                            lsp_evidence(
                                LspEvidenceReadiness::Partial,
                                &EnrichmentScope::ChangedFiles.stable_key(),
                                0,
                                None,
                                Some(detail.clone()),
                                lsp_validations.clone(),
                            ),
                        );
                    }
                    super::sentinel::clear_lsp_sentinel(&self.repo_root);
                    return Err(
                        e.context("Full persist failed during incremental foreground pipeline")
                    );
                }
                // Persist succeeded -- write extraction sentinel. The LSP sentinel
                // is valid only when this invocation completed LSP and persisted
                // the resulting graph in this block.
                super::sentinel::write_extract_sentinel(&self.repo_root, nodes.len(), edges.len());
                if lsp_stage_completed && lsp_degraded_detail.is_none() {
                    super::sentinel::write_lsp_sentinel(&self.repo_root, nodes.len(), edges.len());
                } else {
                    super::sentinel::clear_lsp_sentinel(&self.repo_root);
                }
                if lsp_stage_completed && let Some(job_id) = incremental_lsp_job_id.as_deref() {
                    if let Some(detail) = lsp_degraded_detail.as_deref() {
                        self.enrichment_jobs.mark_degraded(
                            &self.repo_root,
                            job_id,
                            nodes.len(),
                            edges.len(),
                            detail,
                        );
                    } else {
                        self.enrichment_jobs.mark_completed(
                            &self.repo_root,
                            job_id,
                            nodes.len(),
                            edges.len(),
                        );
                    }
                    self.enrichment_jobs.record_lsp_evidence(
                        &self.repo_root,
                        job_id,
                        lsp_evidence(
                            if lsp_degraded_detail.is_some() {
                                LspEvidenceReadiness::Partial
                            } else {
                                LspEvidenceReadiness::Scoped
                            },
                            &EnrichmentScope::ChangedFiles.stable_key(),
                            0,
                            None,
                            lsp_degraded_detail.clone(),
                            lsp_validations.clone(),
                        ),
                    );
                }
                if let (Some(authorization), Some(plan)) =
                    (cache_authorization.as_ref(), cache_plan.as_ref())
                {
                    let stale_paths = authorization
                        .authorization
                        .deleted_paths
                        .iter()
                        .map(PathBuf::from)
                        .chain(
                            authorization
                                .authorization
                                .renamed_paths
                                .iter()
                                .map(|rename| PathBuf::from(&rename[0])),
                        )
                        .collect::<BTreeSet<_>>();
                    crate::structural_cache::validate_persisted_target(
                        &self.repo_root,
                        &nodes,
                        &edges,
                        &stale_paths,
                    )
                    .await?;
                    let changed_paths = authorization
                        .authorization
                        .changed_paths
                        .iter()
                        .chain(authorization.authorization.added_paths.iter())
                        .chain(authorization.authorization.deleted_paths.iter())
                        .map(PathBuf::from)
                        .chain(authorization.authorization.renamed_paths.iter().flat_map(
                            |rename| [PathBuf::from(&rename[0]), PathBuf::from(&rename[1])],
                        ))
                        .collect::<BTreeSet<_>>();
                    let base_producers = authorization
                        .inherited_by_path
                        .values()
                        .flat_map(|file| file.producer_work_ids.iter().cloned())
                        .collect::<BTreeSet<_>>();
                    let executed_records =
                        crate::extract::lsp::work_items::load_all_records(&self.repo_root)?
                            .into_iter()
                            .filter(|record| {
                                plan.executed_paths.contains(Path::new(&record.file))
                            && !base_producers
                                .contains(&format!("{}:{}", record.job_id, record.item_id))
                            && record.state
                                == crate::extract::lsp::work_items::LspWorkItemState::Completed
                            })
                            .collect::<Vec<_>>();
                    let executed_graph_enrichment_operation_count = executed_records
                        .iter()
                        .map(|record| record.requested_operations.len() as u64)
                        .sum();
                    let executed_producer_work_ids = executed_records
                        .iter()
                        .map(|record| format!("{}:{}", record.job_id, record.item_id))
                        .collect();
                    crate::structural_cache::write_execution(
                        &self.repo_root,
                        &crate::structural_cache::StructuralCacheExecution {
                            schema_version: crate::structural_cache::
                                STRUCTURAL_CACHE_AUTHORIZATION_SCHEMA_VERSION,
                            offline_preprocessing: true,
                            base_archive_sha256: authorization
                                .authorization
                                .base_archive_sha256
                                .clone(),
                            base_sidecar_sha256: authorization
                                .authorization
                                .base_sidecar_sha256
                                .clone(),
                            base_report_digest: authorization
                                .authorization
                                .base_report_digest
                                .clone(),
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
                            invalidated_partitions: authorization
                                .authorization
                                .invalidated_partitions
                                .clone(),
                            escalated_partitions: plan
                                .escalated_partitions
                                .iter()
                                .cloned()
                                .collect(),
                            changed_file_count: changed_paths.len() as u64,
                            invalidated_file_count: plan
                                .executed_paths
                                .difference(&changed_paths)
                                .count() as u64,
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
                                .inherited_readiness_validation_request_count(
                                    &plan.inherited_paths,
                                ),
                            executed_readiness_validation_request_count: lsp_validations
                                .iter()
                                .filter(|validation| validation.method.is_some())
                                .count() as u64,
                            executed_producer_work_ids,
                            closure_edge_count: plan.closure_edge_count as u64,
                            execution_job_id: incremental_lsp_job_id.clone(),
                            digest: String::new(),
                        },
                    )?;
                }
            }
        }

        // Commit scanner state after successful persist.
        scanner.commit_state()?;

        // Phase 5: Summary.
        let (total_node_count, total_edge_count, file_count) = {
            let snap = self.graph.load_full();
            let gs = snap.as_ref().as_ref().unwrap();
            let fc = gs
                .nodes
                .iter()
                .map(|n| n.id.file.to_string_lossy().to_string())
                .collect::<std::collections::HashSet<_>>()
                .len();
            (gs.nodes.len(), gs.edges.len(), fc)
        };
        let encoding_stats = {
            let ss = self.scan_stats.read().unwrap_or_else(|e| e.into_inner());
            let mut agg = crate::extract::EncodingStats::default();
            for es in ss.encoding_stats.values() {
                agg.merge(es);
            }
            agg
        };
        let total_time = pipeline_start.elapsed();
        let mut phases = vec![
            PhaseReport::ran(PhaseKind::Extract, extract_time),
            PhaseReport::ran(PhaseKind::PostPasses, bus_time),
            PhaseReport::ran(
                PhaseKind::PersistGraph,
                total_time.saturating_sub(extract_time + bus_time),
            ),
            PhaseReport::ran(PhaseKind::Total, total_time),
        ];
        if !enrichment.runs_lsp() {
            phases.push(PhaseReport::skipped(
                PhaseKind::Lsp,
                "skipped by scan options",
            ));
        }
        if !enrichment.runs_embeddings() {
            phases.push(PhaseReport::skipped(
                PhaseKind::Embeddings,
                "skipped by scan options",
            ));
        }
        let related_job_ids = incremental_lsp_job_id.iter().cloned().collect::<Vec<_>>();
        let (lsp_state, lsp_detail) = lsp_capability_from_status(
            enrichment,
            self.lsp_status.current_state(),
            self.lsp_status.diagnostic().as_deref(),
            lsp_edge_count,
            !related_job_ids.is_empty(),
        );
        if matches!(lsp_state, CapabilityState::Failed) {
            phases.push(PhaseReport::failed(
                PhaseKind::Lsp,
                bus_time,
                lsp_detail
                    .clone()
                    .unwrap_or_else(|| "call-reference enrichment failed".to_string()),
            ));
        } else if matches!(lsp_state, CapabilityState::Unavailable) {
            phases.push(PhaseReport::unavailable(
                PhaseKind::Lsp,
                lsp_detail
                    .clone()
                    .unwrap_or_else(|| "call-reference enrichment unavailable".to_string()),
            ));
        }
        let report = build_pipeline_operation_report(
            &self.repo_root,
            PipelineReportInput {
                operation: OperationKind::IncrementalRefresh,
                enrichment,
                duration: total_time,
                symbol_count: total_node_count,
                edge_count: total_edge_count,
                file_count,
                lsp_edge_count,
                lsp_state,
                lsp_detail,
                embedding_count: 0,
                embeddings_attached: self.embed_index.load().is_some(),
                phases,
                related_job_ids,
                business_context: self.business_context.clone(),
            },
        );

        let result = PipelineResult {
            node_count: total_node_count,
            edge_count: total_edge_count,
            file_count,
            lsp_edge_count,
            embed_count: 0,
            total_time,
            lsp_entries: vec![],
            encoding_stats,
            report,
        };
        on_progress(&result.format_summary());
        Ok(result)
    }

    /// Full rebuild foreground pipeline (no cache available).
    async fn run_pipeline_foreground_full<F>(
        &self,
        on_progress: F,
        pipeline_start: std::time::Instant,
        enrichment: ScanEnrichmentOptions,
    ) -> anyhow::Result<PipelineResult>
    where
        F: Fn(&str) + Send + Sync,
    {
        // Phase 1: Scan + Extract (reuses build_full_graph without background tasks)
        let t0 = std::time::Instant::now();
        // Phase 2 below owns foreground LSP execution and its durable job/status
        // contract. Keep the initial full build extraction-only for LSP so one
        // invocation cannot abort here and then have a second successful/empty
        // pass overwrite the degraded result.
        let graph_state = self
            .build_full_graph_inner(false, enrichment.without_lsp())
            .await?;
        let scan_extract_time = t0.elapsed();

        let file_count = graph_state
            .nodes
            .iter()
            .map(|n| n.id.file.to_string_lossy().to_string())
            .collect::<std::collections::HashSet<_>>()
            .len();

        on_progress(&format!(
            "Scan+Extract: {} symbols across {} files in {:.1}s",
            graph_state.nodes.len(),
            file_count,
            scan_extract_time.as_secs_f64(),
        ));

        // Store graph state atomically so it is available for queries during embed+LSP.
        {
            let mut idx = crate::graph::index::GraphIndex::new();
            idx.rebuild_from_edges(&graph_state.edges);
            for node in &graph_state.nodes {
                idx.ensure_node(&node.stable_id(), &node.id.kind.to_string());
            }
            self.graph.store(Arc::new(Some(Arc::new(GraphState::new(
                graph_state.nodes.clone(),
                graph_state.edges.clone(),
                idx,
                graph_state.last_scan_completed_at,
                graph_state.detected_frameworks.clone(),
            )))));
        }

        // Phase 2: Embed + LSP enrichment (parallel -- they use independent data stores)
        let embeddable_nodes: Vec<Node> = graph_state
            .nodes
            .iter()
            .filter(|n| n.id.root != "external")
            .cloned()
            .collect();

        let (lsp_job_id, run_lsp_in_bus) = if enrichment.runs_lsp() {
            let server_name = self.lsp_status.server_name();
            if let Some(ref name) = server_name {
                on_progress(&format!("LSP: {} found on PATH", name));
            }
            match self.enrichment_jobs.begin_job(
                &self.repo_root,
                EnrichmentCapability::CallReferences,
                EnrichmentScope::Repo,
                EnrichmentTrigger::ForegroundScan,
                None,
            )? {
                JobStart::Started(job) => {
                    self.lsp_status.set_running();
                    self.enrichment_jobs
                        .mark_running(&self.repo_root, &job.job_id, "lsp");
                    (Some(job.job_id), true)
                }
                JobStart::Joined { existing_job_id } => {
                    on_progress(&format!(
                        "LSP: joined active enrichment job {}; skipping duplicate foreground LSP",
                        existing_job_id
                    ));
                    (Some(existing_job_id), false)
                }
            }
        } else {
            (None, false)
        };

        let embed_repo_root = self.repo_root.clone();
        let embed_index_ref = self.embed_index.clone();
        let embed_business_context = self.business_context.clone();
        let (embed_job_id, run_embed_job) = if enrichment.runs_embeddings() {
            match self.enrichment_jobs.begin_job(
                &self.repo_root,
                EnrichmentCapability::Embeddings,
                EnrichmentScope::Repo,
                EnrichmentTrigger::ForegroundScan,
                None,
            )? {
                JobStart::Started(job) => {
                    self.enrichment_jobs
                        .mark_running(&self.repo_root, &job.job_id, "embedding");
                    (Some(job.job_id), true)
                }
                JobStart::Joined { existing_job_id } => {
                    on_progress(&format!(
                        "Embed: joined active enrichment job {}; skipping duplicate foreground embedding",
                        existing_job_id
                    ));
                    (Some(existing_job_id), false)
                }
            }
        } else {
            (None, false)
        };
        let embed_jobs = Arc::clone(&self.enrichment_jobs);
        let embed_fut = async {
            let t1 = std::time::Instant::now();
            if !run_embed_job {
                return (0, t1.elapsed());
            }
            let count = match EmbeddingIndex::new(&embed_repo_root).await {
                Ok(idx) => {
                    match idx
                        .index_all_with_symbols_and_business_context(
                            &embed_repo_root,
                            &embeddable_nodes,
                            &embed_business_context,
                        )
                        .await
                    {
                        Ok(count) => {
                            if let Some(job_id) = embed_job_id.as_deref() {
                                embed_jobs.mark_persisting(&embed_repo_root, job_id, count, count);
                            }
                            embed_index_ref.store(Arc::new(Some(idx)));
                            if let Some(job_id) = embed_job_id.as_deref() {
                                embed_jobs.mark_completed(&embed_repo_root, job_id, count, count);
                            }
                            count
                        }
                        Err(e) => {
                            tracing::warn!("Embed: failed -- {}", e);
                            if let Some(job_id) = embed_job_id.as_deref() {
                                embed_jobs.mark_failed(&embed_repo_root, job_id, format!("{}", e));
                            }
                            0
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Embed: init failed -- {}", e);
                    if let Some(job_id) = embed_job_id.as_deref() {
                        embed_jobs.mark_failed(&embed_repo_root, job_id, format!("{}", e));
                    }
                    0
                }
            };
            let elapsed = t1.elapsed();
            (count, elapsed)
        };

        on_progress("Enrichment: running pipeline via event bus...");

        // ADR-001 (#583): route through emit_enrichment_pipeline instead of
        // calling EnricherRegistry::enrich_all() directly. The bus runs all
        // post-extraction passes (LSP, subsystem detection, framework detection,
        // import calls, tested_by, etc.) and returns the fully enriched graph.
        let (root_pairs, primary_slug) = self.build_bus_root_pairs();
        let participating_lsp_slugs = root_pairs
            .iter()
            .map(|(slug, _)| slug.clone())
            .collect::<HashSet<_>>();
        let bus_fut = async {
            let t2 = std::time::Instant::now();
            let result = emit_lsp_pipeline_with_budget(LspPipelineInput {
                nodes: graph_state.nodes.clone(),
                edges: graph_state.edges.clone(),
                root_pairs,
                primary_slug,
                repo_root: self.repo_root.clone(),
                scan_stats: Arc::clone(&self.scan_stats),
                skip_lsp: !run_lsp_in_bus,
                dirty_slugs: None,
                lsp_node_filter: None,
                file_readiness_filter: None,
                broad_reference_budget: None,
            })
            .await;
            let elapsed = t2.elapsed();
            (result, elapsed)
        };

        let mut lsp_stage_completed = false;
        let mut lsp_degraded_detail = None;
        let lsp_validations;
        let ((embed_count, embed_time), (bus_result, bus_time)) = tokio::join!(embed_fut, bus_fut);

        on_progress(&format!(
            "Embed: {} items in {:.1}s",
            embed_count,
            embed_time.as_secs_f64(),
        ));

        let lsp_edge_count;

        match bus_result {
            Ok((
                mut enriched_nodes,
                mut enriched_edges,
                detected_frameworks,
                diagnostics,
                bus_validations,
            )) => {
                lsp_validations = bus_validations;
                // Dedup: passes can re-emit cached entries.
                {
                    let mut seen_nodes = std::collections::HashSet::new();
                    enriched_nodes.reverse();
                    enriched_nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                    enriched_nodes.reverse();

                    dedup_edges_preserving_lsp_evidence(&mut enriched_edges);
                }

                lsp_edge_count = enriched_edges
                    .iter()
                    .filter(|e| {
                        e.source == crate::graph::ExtractionSource::Lsp
                            && matches!(e.kind, crate::graph::EdgeKind::Calls)
                    })
                    .count();
                if let Some(job_id) = lsp_job_id.as_deref() {
                    self.enrichment_jobs.mark_progress(
                        &self.repo_root,
                        job_id,
                        "lsp_edges",
                        lsp_edge_count,
                        None,
                    );
                }
                let lsp_abort_detail = if run_lsp_in_bus {
                    (!diagnostics.is_empty())
                        .then(|| diagnostics.join("; "))
                        .or_else(|| {
                            let lsp_failures = self
                                .scan_stats
                                .read()
                                .map(|stats| {
                                    lsp_abort_failures_for_slugs(&stats, &participating_lsp_slugs)
                                })
                                .unwrap_or_else(|_| {
                                    vec!["scan stats unavailable: lock poisoned".to_string()]
                                });
                            (!lsp_failures.is_empty()).then(|| {
                                format!(
                                    "LSP call-reference enrichment aborted: {}",
                                    lsp_failures.join("; ")
                                )
                            })
                        })
                } else {
                    None
                };
                if let Some(detail) = lsp_abort_detail.as_deref() {
                    on_progress(&format!(
                        "Enrichment: finalized with degraded LSP output: {detail}"
                    ));
                }

                on_progress(&format!(
                    "Enrichment: {} LSP call edges via bus in {:.1}s",
                    lsp_edge_count,
                    bus_time.as_secs_f64(),
                ));

                // Build updated index and apply enriched graph via atomic swap.
                let mut new_index = crate::graph::index::GraphIndex::new();
                new_index.rebuild_from_edges(&enriched_edges);
                for node in &enriched_nodes {
                    new_index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                }

                {
                    let snap = self.graph.load_full();
                    if let Some(ref current_gs) = *snap {
                        let mut gs = (**current_gs).clone();
                        gs.nodes = enriched_nodes;
                        gs.edges = enriched_edges;
                        gs.index = new_index;
                        gs.detected_frameworks = detected_frameworks;
                        self.graph.store(Arc::new(Some(Arc::new(gs))));
                    }
                }
                if run_lsp_in_bus {
                    if let Some(detail) = lsp_abort_detail.as_deref() {
                        self.lsp_status.set_degraded(lsp_edge_count, detail);
                        lsp_degraded_detail = Some(detail.to_string());
                    } else {
                        self.lsp_status
                            .set_complete_default_profile_for_warmup(lsp_edge_count);
                    }
                    lsp_stage_completed = true;
                }
            }
            Err(e) => {
                tracing::error!(
                    "Foreground full pipeline: emit_enrichment_pipeline failed: {:#}",
                    e
                );
                on_progress(&format!(
                    "Enrichment: pipeline failed in {:.1}s -- graph has tree-sitter data only",
                    bus_time.as_secs_f64(),
                ));
                if run_lsp_in_bus {
                    if e.is_timeout() {
                        self.lsp_status.set_timed_out(&format!("{}", e));
                    } else {
                        self.lsp_status.set_unavailable();
                    }
                }
                if let Some(job_id) = lsp_job_id.as_deref() {
                    if e.is_timeout() {
                        self.enrichment_jobs.mark_timed_out(
                            &self.repo_root,
                            job_id,
                            format!("{}", e),
                        );
                    } else {
                        self.enrichment_jobs
                            .mark_failed(&self.repo_root, job_id, format!("{}", e));
                    }
                }
                return Err(e.into());
            }
        }

        // Phase 3: Full persist — write the complete graph (tree-sitter + LSP edges)
        // to LanceDB. build_full_graph_inner(false) deferred persistence so we can
        // include LSP edges in a single atomic write (#311).
        {
            let snapshot = {
                let snap = self.graph.load_full();
                snap.as_ref()
                    .as_ref()
                    .map(|gs| (gs.nodes.clone(), gs.edges.clone()))
            };
            if let Some((nodes, edges)) = snapshot {
                tracing::info!(
                    "Foreground full persist: {} nodes, {} edges (including {} LSP)",
                    nodes.len(),
                    edges.len(),
                    lsp_edge_count,
                );
                if lsp_stage_completed && let Some(job_id) = lsp_job_id.as_deref() {
                    self.enrichment_jobs.mark_persisting(
                        &self.repo_root,
                        job_id,
                        nodes.len(),
                        edges.len(),
                    );
                }
                if let Err(e) = persist_graph_to_lance(&self.repo_root, &nodes, &edges).await {
                    tracing::error!("Foreground full persist failed: {}", e);
                    if lsp_stage_completed && let Some(job_id) = lsp_job_id.as_deref() {
                        let detail = format!("Full persist failed during foreground pipeline: {e}");
                        self.enrichment_jobs
                            .mark_failed(&self.repo_root, job_id, detail.clone());
                        self.enrichment_jobs.record_lsp_evidence(
                            &self.repo_root,
                            job_id,
                            lsp_evidence(
                                LspEvidenceReadiness::Partial,
                                &EnrichmentScope::Repo.stable_key(),
                                0,
                                None,
                                Some(detail.clone()),
                                lsp_validations.clone(),
                            ),
                        );
                    }
                    super::sentinel::clear_lsp_sentinel(&self.repo_root);
                    return Err(e.context("Full persist failed during foreground pipeline"));
                }
                // Full persist succeeded -- write extraction sentinel. The LSP sentinel
                // is valid only when this invocation completed LSP and persisted
                // the resulting graph in this block.
                super::sentinel::write_extract_sentinel(&self.repo_root, nodes.len(), edges.len());
                if lsp_stage_completed && lsp_degraded_detail.is_none() {
                    super::sentinel::write_lsp_sentinel(&self.repo_root, nodes.len(), edges.len());
                } else {
                    super::sentinel::clear_lsp_sentinel(&self.repo_root);
                }
                if lsp_stage_completed && let Some(job_id) = lsp_job_id.as_deref() {
                    if let Some(detail) = lsp_degraded_detail.as_deref() {
                        self.enrichment_jobs.mark_degraded(
                            &self.repo_root,
                            job_id,
                            nodes.len(),
                            edges.len(),
                            detail,
                        );
                    } else {
                        self.enrichment_jobs.mark_completed(
                            &self.repo_root,
                            job_id,
                            nodes.len(),
                            edges.len(),
                        );
                    }
                    self.enrichment_jobs.record_lsp_evidence(
                        &self.repo_root,
                        job_id,
                        lsp_evidence(
                            if lsp_degraded_detail.is_some() {
                                LspEvidenceReadiness::Partial
                            } else {
                                LspEvidenceReadiness::DefaultProfile
                            },
                            &EnrichmentScope::Repo.stable_key(),
                            0,
                            None,
                            lsp_degraded_detail.clone().or_else(|| {
                                Some(
                                    "repo-wide default query profile completed; broad references were omitted"
                                        .to_string(),
                                )
                            }),
                            lsp_validations.clone(),
                        ),
                    );
                }
            }
        }

        // Phase 4: Summary
        let (total_node_count, total_edge_count) = {
            let snap = self.graph.load_full();
            match snap.as_ref().as_ref() {
                Some(gs) => (gs.nodes.len(), gs.edges.len()),
                None => (graph_state.nodes.len(), graph_state.edges.len()),
            }
        };
        let encoding_stats = {
            let ss = self.scan_stats.read().unwrap_or_else(|e| e.into_inner());
            let mut agg = crate::extract::EncodingStats::default();
            for es in ss.encoding_stats.values() {
                agg.merge(es);
            }
            agg
        };
        let total_time = pipeline_start.elapsed();
        let related_job_ids = [lsp_job_id.clone(), embed_job_id.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let (lsp_state, lsp_detail) = lsp_capability_from_status(
            enrichment,
            self.lsp_status.current_state(),
            self.lsp_status.diagnostic().as_deref(),
            lsp_edge_count,
            !related_job_ids.is_empty(),
        );
        let mut phases = vec![
            PhaseReport::ran(PhaseKind::Extract, scan_extract_time),
            PhaseReport::ran(PhaseKind::PostPasses, bus_time),
            PhaseReport::ran(PhaseKind::Total, total_time),
        ];
        if enrichment.runs_embeddings() {
            phases.push(PhaseReport::ran(PhaseKind::Embeddings, embed_time));
        } else {
            phases.push(PhaseReport::skipped(
                PhaseKind::Embeddings,
                "skipped by scan options",
            ));
        }
        if matches!(lsp_state, CapabilityState::Failed) {
            phases.push(PhaseReport::failed(
                PhaseKind::Lsp,
                bus_time,
                lsp_detail
                    .clone()
                    .unwrap_or_else(|| "call-reference enrichment failed".to_string()),
            ));
        } else if matches!(lsp_state, CapabilityState::Unavailable) {
            phases.push(PhaseReport::unavailable(
                PhaseKind::Lsp,
                lsp_detail
                    .clone()
                    .unwrap_or_else(|| "call-reference enrichment unavailable".to_string()),
            ));
        }
        if !enrichment.runs_lsp() {
            phases.push(PhaseReport::skipped(
                PhaseKind::Lsp,
                "skipped by scan options",
            ));
        }
        let report = build_pipeline_operation_report(
            &self.repo_root,
            PipelineReportInput {
                operation: OperationKind::FullRebuild,
                enrichment,
                duration: total_time,
                symbol_count: total_node_count,
                edge_count: total_edge_count,
                file_count,
                lsp_edge_count,
                lsp_state,
                lsp_detail,
                embedding_count: embed_count,
                embeddings_attached: self.embed_index.load().is_some(),
                phases,
                related_job_ids,
                business_context: self.business_context.clone(),
            },
        );

        let result = PipelineResult {
            node_count: total_node_count,
            edge_count: total_edge_count,
            file_count,
            lsp_edge_count,
            embed_count,
            total_time,
            lsp_entries: vec![],
            encoding_stats,
            report,
        };
        on_progress(&result.format_summary());
        Ok(result)
    }

    /// Run enrichment pipeline on the full graph synchronously and persist.
    /// Used when the cached graph has no LSP sentinel and needs enrichment.
    /// Routes through `emit_enrichment_pipeline` per ADR-001 (#583).
    /// Returns the number of LSP call edges added.
    async fn run_foreground_lsp_and_persist<F>(
        &self,
        on_progress: &F,
        request: ForegroundLspRequest,
    ) -> anyhow::Result<LspEnrichmentRun>
    where
        F: Fn(&str) + Send + Sync,
    {
        let ForegroundLspRequest {
            scope,
            trigger,
            dirty_slugs,
            node_filter: lsp_node_filter,
            fail_on_lsp_error,
            broad_reference_budget,
            declared_node_count,
        } = request;
        let (all_nodes, mut all_edges) = {
            let snap = self.graph.load_full();
            let gs = snap.as_ref().as_ref().unwrap();
            (gs.nodes.clone(), gs.edges.clone())
        };
        let existing_node_ids = lsp_node_filter
            .as_ref()
            .map(|_| {
                all_nodes
                    .iter()
                    .map(Node::stable_id)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let existing_edge_ids = lsp_node_filter
            .as_ref()
            .map(|_| {
                all_edges
                    .iter()
                    .map(Edge::stable_id)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let removed_scoped_lsp_edge_ids = lsp_node_filter
            .as_deref()
            .map(|node_filter| remove_existing_scoped_lsp_edges(&mut all_edges, node_filter))
            .unwrap_or_default();
        let persistence_node_filter = lsp_node_filter.clone();
        let repo_wide_lsp = matches!(scope, EnrichmentScope::Repo);
        let scope_detail = scope.stable_key();

        let server_name = self.lsp_status.server_name();
        if let Some(ref name) = server_name {
            on_progress(&format!("LSP: {} found on PATH", name));
        }
        let job_id = match self.enrichment_jobs.begin_job(
            &self.repo_root,
            EnrichmentCapability::CallReferences,
            scope,
            trigger,
            None,
        )? {
            JobStart::Started(job) => {
                self.lsp_status.set_running();
                self.enrichment_jobs
                    .mark_running(&self.repo_root, &job.job_id, "lsp");
                job.job_id
            }
            JobStart::Joined { existing_job_id } => {
                on_progress(&format!(
                    "LSP: joined active enrichment job {}; waiting for completion",
                    existing_job_id
                ));
                let edge_count = self
                    .wait_for_joined_enrichment_job(
                        &existing_job_id,
                        EnrichmentCapability::CallReferences,
                    )
                    .await?;
                return Ok(LspEnrichmentRun {
                    edge_count,
                    job_id: existing_job_id,
                });
            }
        };

        on_progress("Enrichment: running pipeline via event bus (no sentinel)...");

        let (root_pairs, primary_slug) = self.build_bus_root_pairs();
        let participating_lsp_slugs = dirty_slugs.clone().unwrap_or_else(|| {
            root_pairs
                .iter()
                .map(|(slug, _)| slug.clone())
                .collect::<HashSet<_>>()
        });
        let bus_result = emit_lsp_pipeline_with_budget(LspPipelineInput {
            nodes: all_nodes,
            edges: all_edges,
            root_pairs,
            primary_slug,
            repo_root: self.repo_root.clone(),
            scan_stats: Arc::clone(&self.scan_stats),
            skip_lsp: false,
            dirty_slugs,
            lsp_node_filter,
            file_readiness_filter: None,
            broad_reference_budget: broad_reference_budget.clone(),
        })
        .await;

        match bus_result {
            Ok((
                mut enriched_nodes,
                mut enriched_edges,
                detected_frameworks,
                diagnostics,
                lsp_validations,
            )) => {
                // Dedup: passes can re-emit cached entries.
                {
                    let mut seen_nodes = std::collections::HashSet::new();
                    enriched_nodes.reverse();
                    enriched_nodes.retain(|n| seen_nodes.insert(n.stable_id()));
                    enriched_nodes.reverse();

                    dedup_edges_preserving_lsp_evidence(&mut enriched_edges);
                }

                let scoped_delta = persistence_node_filter.as_deref().map(|node_filter| {
                    scoped_lsp_persistence_delta(
                        &enriched_nodes,
                        &enriched_edges,
                        node_filter,
                        &existing_node_ids,
                        &existing_edge_ids,
                        removed_scoped_lsp_edge_ids,
                    )
                });
                let lsp_edge_count = scoped_delta
                    .as_ref()
                    .map(|delta| {
                        delta
                            .upsert_edges
                            .iter()
                            .filter(|edge| matches!(edge.kind, crate::graph::EdgeKind::Calls))
                            .count()
                    })
                    .unwrap_or_else(|| {
                        enriched_edges
                            .iter()
                            .filter(|edge| {
                                edge.source == crate::graph::ExtractionSource::Lsp
                                    && matches!(edge.kind, crate::graph::EdgeKind::Calls)
                            })
                            .count()
                    });

                let budget_snapshot = broad_reference_budget
                    .as_ref()
                    .map(|budget| budget.snapshot());
                let budget_abort_detail = budget_snapshot
                    .as_ref()
                    .filter(|snapshot| snapshot.circuit_open)
                    .and_then(|snapshot| snapshot.circuit_reason.clone());
                let lsp_abort_detail = (!diagnostics.is_empty())
                    .then(|| diagnostics.join("; "))
                    .or(budget_abort_detail)
                    .or_else(|| {
                        let stats = self.scan_stats.read().unwrap_or_else(|e| e.into_inner());
                        lsp_abort_failures_for_slugs(&stats, &participating_lsp_slugs)
                            .into_iter()
                            .next()
                            .map(|failure| format!("LSP enrichment aborted for {failure}"))
                    });
                if let Some(detail) = lsp_abort_detail.as_deref() {
                    on_progress(&format!("Enrichment: {detail}"));
                }

                on_progress(&format!(
                    "Enrichment: {} LSP call edges via bus",
                    lsp_edge_count
                ));

                // Build updated index and apply enriched graph via atomic swap.
                let mut new_index = crate::graph::index::GraphIndex::new();
                new_index.rebuild_from_edges(&enriched_edges);
                for node in &enriched_nodes {
                    new_index.ensure_node(&node.stable_id(), &node.id.kind.to_string());
                }

                {
                    let snap = self.graph.load_full();
                    if let Some(ref current_gs) = *snap {
                        let mut gs = (**current_gs).clone();
                        gs.nodes = enriched_nodes.clone();
                        gs.edges = enriched_edges.clone();
                        gs.index = new_index;
                        gs.detected_frameworks = detected_frameworks;
                        self.graph.store(Arc::new(Some(Arc::new(gs))));
                    }
                }
                if let Some(detail) = lsp_abort_detail.as_deref() {
                    if repo_wide_lsp {
                        self.lsp_status.set_degraded(lsp_edge_count, detail);
                    } else {
                        let existing_coverage = self.lsp_status.coverage_edge_count();
                        self.lsp_status.set_degraded_scoped(
                            lsp_edge_count,
                            existing_coverage,
                            scope_detail.clone(),
                            detail,
                        );
                    }
                } else if repo_wide_lsp {
                    self.lsp_status.set_complete_default_profile(
                        lsp_edge_count,
                        lsp_edge_count,
                        "broad references were omitted; request changed, targets, or task scope for broad evidence",
                    );
                } else {
                    let existing_coverage = self.lsp_status.coverage_edge_count();
                    self.lsp_status.set_complete_scoped(
                        lsp_edge_count,
                        existing_coverage,
                        scope_detail.clone(),
                    );
                }

                self.enrichment_jobs.mark_progress(
                    &self.repo_root,
                    &job_id,
                    "lsp_edges",
                    lsp_edge_count,
                    None,
                );
                let (mut persisted_node_count, mut persisted_edge_count) = scoped_delta
                    .as_ref()
                    .map(|delta| (delta.upsert_nodes.len(), delta.upsert_edges.len()))
                    .unwrap_or((enriched_nodes.len(), enriched_edges.len()));
                self.enrichment_jobs.mark_persisting(
                    &self.repo_root,
                    &job_id,
                    persisted_node_count,
                    persisted_edge_count,
                );
                let persist_result = if let Some(delta) = scoped_delta.as_ref() {
                    match persist_graph_incremental(
                        &self.repo_root,
                        &delta.upsert_nodes,
                        &delta.upsert_edges,
                        &delta.deleted_edge_ids,
                        &[],
                    )
                    .await
                    {
                        Ok(true) => {
                            // Schema migration drops the old tables, so the incremental seam
                            // explicitly requires a one-time full reconstruction.
                            persisted_node_count = enriched_nodes.len();
                            persisted_edge_count = enriched_edges.len();
                            self.enrichment_jobs.mark_persisting(
                                &self.repo_root,
                                &job_id,
                                persisted_node_count,
                                persisted_edge_count,
                            );
                            persist_graph_to_lance(
                                &self.repo_root,
                                &enriched_nodes,
                                &enriched_edges,
                            )
                            .await
                        }
                        Ok(false) => Ok(()),
                        Err(error) => Err(error),
                    }
                } else {
                    persist_graph_to_lance(&self.repo_root, &enriched_nodes, &enriched_edges).await
                };
                if let Err(e) = persist_result {
                    tracing::error!("Foreground LSP persist failed: {}", e);
                    self.enrichment_jobs
                        .mark_failed(&self.repo_root, &job_id, format!("{}", e));
                    self.enrichment_jobs.record_lsp_evidence(
                        &self.repo_root,
                        &job_id,
                        lsp_evidence(
                            LspEvidenceReadiness::Partial,
                            &scope_detail,
                            declared_node_count,
                            budget_snapshot.as_ref(),
                            Some(format!("persistence failed: {e}")),
                            lsp_validations.clone(),
                        ),
                    );
                    return Err(e.context("LSP persist failed during foreground pipeline"));
                }
                // Persist succeeded -- write LSP sentinel so future startups know
                // LSP enrichment is durable and can skip re-enrichment (#477).
                if repo_wide_lsp && lsp_abort_detail.is_none() {
                    super::sentinel::write_lsp_sentinel(
                        &self.repo_root,
                        enriched_nodes.len(),
                        enriched_edges.len(),
                    );
                } else if lsp_abort_detail.is_some() {
                    super::sentinel::clear_lsp_sentinel(&self.repo_root);
                }
                if let Some(detail) = lsp_abort_detail.as_deref() {
                    self.enrichment_jobs.mark_degraded(
                        &self.repo_root,
                        &job_id,
                        persisted_node_count,
                        persisted_edge_count,
                        detail,
                    );
                } else {
                    self.enrichment_jobs.mark_completed(
                        &self.repo_root,
                        &job_id,
                        persisted_node_count,
                        persisted_edge_count,
                    );
                }
                self.enrichment_jobs.record_lsp_evidence(
                    &self.repo_root,
                    &job_id,
                    lsp_evidence(
                        if lsp_abort_detail.is_some() {
                            LspEvidenceReadiness::Partial
                        } else if repo_wide_lsp {
                            LspEvidenceReadiness::DefaultProfile
                        } else {
                            LspEvidenceReadiness::Scoped
                        },
                        &scope_detail,
                        declared_node_count,
                        budget_snapshot.as_ref(),
                        lsp_abort_detail.clone().or_else(|| {
                            repo_wide_lsp.then(|| {
                                "repo-wide default query profile completed; broad references were omitted"
                                    .to_string()
                            })
                        }),
                        lsp_validations,
                    ),
                );

                if fail_on_lsp_error && let Some(detail) = lsp_abort_detail {
                    return Err(anyhow::anyhow!(
                        "LSP enrichment finalized with degraded output: {detail}"
                    ));
                }

                Ok(LspEnrichmentRun {
                    edge_count: lsp_edge_count,
                    job_id,
                })
            }
            Err(e) => {
                tracing::error!(
                    "Foreground LSP pipeline: emit_enrichment_pipeline failed: {:#}",
                    e
                );
                on_progress("Enrichment: pipeline failed -- no LSP edges available");
                if e.is_timeout() {
                    self.lsp_status.set_timed_out(&format!("{}", e));
                    self.enrichment_jobs
                        .mark_timed_out(&self.repo_root, &job_id, format!("{}", e));
                } else {
                    self.lsp_status.set_unavailable();
                    self.enrichment_jobs
                        .mark_failed(&self.repo_root, &job_id, format!("{}", e));
                }
                let budget_snapshot = broad_reference_budget
                    .as_ref()
                    .map(|budget| budget.snapshot());
                self.enrichment_jobs.record_lsp_evidence(
                    &self.repo_root,
                    &job_id,
                    lsp_evidence(
                        LspEvidenceReadiness::Unavailable,
                        &scope_detail,
                        declared_node_count,
                        budget_snapshot.as_ref(),
                        Some(e.to_string()),
                        Vec::new(),
                    ),
                );
                if fail_on_lsp_error {
                    return Err(anyhow::anyhow!("LSP enrichment failed: {}", e));
                }
                Ok(LspEnrichmentRun {
                    edge_count: 0,
                    job_id,
                })
            }
        }
    }

    pub async fn run_explicit_enrichment<F>(
        &self,
        capability: EnrichmentCapability,
        scope: EnrichmentScope,
        continuation: EnrichmentContinuation,
        on_progress: F,
    ) -> anyhow::Result<Vec<String>>
    where
        F: Fn(&str) + Send + Sync,
    {
        self.run_explicit_enrichment_with_broad_reference_budget(
            capability,
            scope,
            continuation,
            None,
            on_progress,
        )
        .await
    }

    pub async fn run_explicit_enrichment_with_broad_reference_budget<F>(
        &self,
        capability: EnrichmentCapability,
        scope: EnrichmentScope,
        continuation: EnrichmentContinuation,
        broad_reference_budget: Option<BroadReferenceBudget>,
        on_progress: F,
    ) -> anyhow::Result<Vec<String>>
    where
        F: Fn(&str) + Send + Sync,
    {
        let (all_nodes, _all_edges) = {
            let snap = self.graph.load_full();
            let Some(gs) = snap.as_ref().as_ref() else {
                anyhow::bail!(
                    "no cached graph loaded; run `repo-native-alignment scan --extract-only --repo {}` first",
                    self.repo_root.display()
                );
            };
            (gs.nodes.clone(), gs.edges.clone())
        };
        let mut related_job_ids = Vec::new();

        match capability {
            EnrichmentCapability::Embeddings => {
                related_job_ids.extend(
                    self.run_explicit_embedding_enrichment(
                        &all_nodes,
                        scope.clone(),
                        continuation,
                        &on_progress,
                    )
                    .await?,
                );
            }
            EnrichmentCapability::CallReferences => {
                let is_broad_scope = matches!(
                    scope,
                    EnrichmentScope::ChangedFiles
                        | EnrichmentScope::TargetSymbols(_)
                        | EnrichmentScope::TaskRelevant { .. }
                );
                let runtime_budget = if is_broad_scope {
                    let budget = broad_reference_budget
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "explicit broad-reference scope requires a visible request/time budget"
                            )
                        })?
                        .validate()?;
                    Some(Arc::new(crate::extract::lsp::LspBroadReferenceBudget::new(
                        budget.max_requests,
                        Duration::from_millis(budget.max_duration_ms),
                    )))
                } else {
                    None
                };
                let lsp_node_filter = match &scope {
                    EnrichmentScope::ChangedFiles => {
                        let root_slug = RootConfig::code_project(self.repo_root.clone()).slug();
                        let plan = discover_and_plan_changed_files_with_broad_references(
                            &self.repo_root,
                            &root_slug,
                            &all_nodes,
                        )?;
                        for line in plan.render_progress() {
                            on_progress(&line);
                        }
                        Some(plan.planned_node_ids())
                    }
                    EnrichmentScope::TargetSymbols(_) | EnrichmentScope::TaskRelevant { .. } => {
                        Some(plan_explicit_lsp_scope(&all_nodes, &scope)?)
                    }
                    _ => None,
                };
                let declared_node_count = lsp_node_filter.as_ref().map_or(0, |filter| filter.len());
                if let Some(budget) = runtime_budget.as_ref() {
                    let snapshot = budget.snapshot();
                    on_progress(&format!(
                        "Broad-reference contract: scope={} nodes={} max_requests={} max_duration_ms={}",
                        scope.stable_key(),
                        declared_node_count,
                        snapshot.max_requests,
                        snapshot.max_duration_ms
                    ));
                }
                let dirty_slugs = self.dirty_slugs_for_scope(&scope);
                let run = self
                    .run_foreground_lsp_and_persist(
                        &on_progress,
                        ForegroundLspRequest {
                            scope: scope.clone(),
                            trigger: EnrichmentTrigger::Explicit,
                            dirty_slugs,
                            node_filter: lsp_node_filter,
                            fail_on_lsp_error: true,
                            broad_reference_budget: runtime_budget,
                            declared_node_count,
                        },
                    )
                    .await?;
                related_job_ids.push(run.job_id);
                on_progress(&format!(
                    "LSP explicit enrichment complete: {} call/reference edges",
                    run.edge_count
                ));
                if should_continue_lsp_enrichment(&scope, continuation) {
                    match continuation {
                        EnrichmentContinuation::Disabled => {}
                        EnrichmentContinuation::SpawnBackground => {
                            let (current_nodes, current_edges) = {
                                let snap = self.graph.load_full();
                                let Some(gs) = snap.as_ref().as_ref() else {
                                    anyhow::bail!(
                                        "graph cache disappeared before LSP background continuation"
                                    );
                                };
                                (gs.nodes.clone(), gs.edges.clone())
                            };
                            self.spawn_lsp_enrichment_via_bus(&current_nodes, &current_edges);
                            on_progress(
                                "LSP background continuation scheduled for remaining cached graph coverage",
                            );
                        }
                        EnrichmentContinuation::RunToCompletion => {
                            on_progress(
                                "LSP continuation: enriching remaining cached graph coverage",
                            );
                            let continuation_run = self
                                .run_foreground_lsp_and_persist(
                                    &on_progress,
                                    ForegroundLspRequest {
                                        scope: EnrichmentScope::Repo,
                                        trigger: EnrichmentTrigger::Explicit,
                                        dirty_slugs: None,
                                        node_filter: None,
                                        fail_on_lsp_error: true,
                                        broad_reference_budget: None,
                                        declared_node_count: 0,
                                    },
                                )
                                .await?;
                            related_job_ids.push(continuation_run.job_id);
                            on_progress(&format!(
                                "LSP continuation complete: {} call/reference edges",
                                continuation_run.edge_count
                            ));
                        }
                    }
                }
            }
            EnrichmentCapability::ExtractedGraph => {
                anyhow::bail!(
                    "explicit enrich does not extract source; run `repo-native-alignment scan --extract-only --repo {}`",
                    self.repo_root.display()
                );
            }
        }

        Ok(related_job_ids)
    }

    async fn run_explicit_embedding_enrichment<F>(
        &self,
        all_nodes: &[Node],
        scope: EnrichmentScope,
        continuation: EnrichmentContinuation,
        on_progress: &F,
    ) -> anyhow::Result<Vec<String>>
    where
        F: Fn(&str) + Send + Sync,
    {
        let mut related_job_ids = vec![
            self.run_embedding_enrichment_once(all_nodes, scope.clone(), on_progress)
                .await?,
        ];

        if continuation.enabled() && !matches!(scope, EnrichmentScope::Repo) {
            match continuation {
                EnrichmentContinuation::Disabled => {}
                EnrichmentContinuation::SpawnBackground => {
                    let handle = self.spawn_background_enrichment(all_nodes);
                    *self.embed_handle.lock().await = Some(handle);
                    on_progress(
                        "Embedding background continuation scheduled for remaining cached graph coverage",
                    );
                }
                EnrichmentContinuation::RunToCompletion => {
                    on_progress(
                        "Embedding continuation: enriching remaining cached graph coverage",
                    );
                    related_job_ids.push(
                        self.run_embedding_enrichment_once(
                            all_nodes,
                            EnrichmentScope::Repo,
                            on_progress,
                        )
                        .await?,
                    );
                }
            }
        }

        Ok(related_job_ids)
    }

    async fn run_embedding_enrichment_once<F>(
        &self,
        all_nodes: &[Node],
        scope: EnrichmentScope,
        on_progress: &F,
    ) -> anyhow::Result<String>
    where
        F: Fn(&str) + Send + Sync,
    {
        let selected_nodes = self.cached_nodes_for_scope(all_nodes, &scope)?;
        let embeddable_count = selected_nodes
            .iter()
            .filter(|n| n.id.kind.is_embeddable())
            .count();
        let job_id = match self.enrichment_jobs.begin_job(
            &self.repo_root,
            EnrichmentCapability::Embeddings,
            scope.clone(),
            EnrichmentTrigger::Explicit,
            None,
        )? {
            JobStart::Started(job) => job.job_id,
            JobStart::Joined { existing_job_id } => {
                on_progress(&format!(
                    "Embed: joined active enrichment job {}; waiting for completion",
                    existing_job_id
                ));
                self.wait_for_joined_enrichment_job(
                    &existing_job_id,
                    EnrichmentCapability::Embeddings,
                )
                .await?;
                return Ok(existing_job_id);
            }
        };

        self.enrichment_jobs
            .mark_running(&self.repo_root, &job_id, "embedding");
        self.embed_status.set_building(embeddable_count);
        self.enrichment_jobs.mark_progress(
            &self.repo_root,
            &job_id,
            "embedding",
            0,
            Some(embeddable_count),
        );

        let result = async {
            let idx = EmbeddingIndex::new(&self.repo_root).await?;
            if !matches!(scope, EnrichmentScope::Repo) && !idx.has_table().await? {
                anyhow::bail!(
                    "embedding table is missing; run `repo-native-alignment enrich --capability embeddings --scope repo --repo {}` before scoped embedding enrichment",
                    self.repo_root.display()
                );
            }
            let count = if matches!(scope, EnrichmentScope::Repo) {
                idx.index_all_with_symbols_and_business_context(
                    &self.repo_root,
                    &selected_nodes,
                    &self.business_context,
                )
                .await?
            } else {
                idx.reindex_nodes(&selected_nodes).await?
            };
            anyhow::Ok((idx, count))
        }
        .await;

        match result {
            Ok((idx, count)) => {
                self.enrichment_jobs
                    .mark_persisting(&self.repo_root, &job_id, count, count);
                self.embed_index.store(Arc::new(Some(idx)));
                self.embed_status.set_complete(count);
                self.enrichment_jobs
                    .mark_completed(&self.repo_root, &job_id, count, count);
                on_progress(&format!(
                    "Embed explicit enrichment complete: {} embedded items",
                    count
                ));
                Ok(job_id)
            }
            Err(e) => {
                self.embed_status.set_failed(format!("{}", e));
                self.enrichment_jobs
                    .mark_failed(&self.repo_root, &job_id, format!("{}", e));
                Err(e)
            }
        }
    }

    async fn wait_for_joined_enrichment_job(
        &self,
        job_id: &str,
        capability: EnrichmentCapability,
    ) -> anyhow::Result<usize> {
        let budget = LspBudget::from_env().max_duration;
        let started = std::time::Instant::now();
        loop {
            if let Some(job) = self
                .enrichment_jobs
                .recent_jobs(&self.repo_root, 100)
                .into_iter()
                .find(|job| job.job_id == job_id)
            {
                match job.state {
                    EnrichmentJobState::Completed => {
                        if capability == EnrichmentCapability::CallReferences
                            && let Some(progress) = self
                                .enrichment_jobs
                                .events_for_job(&self.repo_root, job_id)
                                .into_iter()
                                .rev()
                                .find(|event| event.phase.as_deref() == Some("lsp_edges"))
                        {
                            return Ok(progress.counters.current);
                        }
                        return Ok(job.counters.current);
                    }
                    EnrichmentJobState::Degraded => {
                        let detail = job.failure.unwrap_or_else(|| {
                            "call-reference enrichment finalized with degraded output".to_string()
                        });
                        anyhow::bail!("joined enrichment job {} degraded: {}", job_id, detail);
                    }
                    EnrichmentJobState::Failed => {
                        let detail = job.failure.unwrap_or_else(|| "unknown failure".to_string());
                        anyhow::bail!("joined enrichment job {} failed: {}", job_id, detail);
                    }
                    EnrichmentJobState::Superseded => {
                        anyhow::bail!("joined enrichment job {} was superseded", job_id);
                    }
                    EnrichmentJobState::Cancelled => {
                        anyhow::bail!("joined enrichment job {} was cancelled", job_id);
                    }
                    EnrichmentJobState::Queued
                    | EnrichmentJobState::Running
                    | EnrichmentJobState::Persisting => {}
                }
            }

            if started.elapsed() > budget {
                anyhow::bail!(
                    "timed out waiting for joined enrichment job {} after {}s",
                    job_id,
                    budget.as_secs()
                );
            }

            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    fn cached_nodes_for_scope(
        &self,
        all_nodes: &[Node],
        scope: &EnrichmentScope,
    ) -> anyhow::Result<Vec<Node>> {
        let nodes: Vec<Node> = match scope {
            EnrichmentScope::Repo => all_nodes
                .iter()
                .filter(|n| n.id.root != "external")
                .cloned()
                .collect(),
            EnrichmentScope::Root(root) => all_nodes
                .iter()
                .filter(|n| n.id.root == *root && n.id.root != "external")
                .cloned()
                .collect(),
            EnrichmentScope::ChangedFiles => {
                let scan = Scanner::new(self.repo_root.clone())?.scan()?;
                let changed: HashSet<String> = scan
                    .changed_files
                    .iter()
                    .chain(scan.new_files.iter())
                    .map(|path| path.to_string_lossy().to_string())
                    .collect();
                all_nodes
                    .iter()
                    .filter(|node| changed.contains(&node.id.file.to_string_lossy().to_string()))
                    .cloned()
                    .collect()
            }
            EnrichmentScope::TargetSymbols(_) | EnrichmentScope::TaskRelevant { .. } => {
                let planned = plan_explicit_lsp_scope(all_nodes, scope)?;
                all_nodes
                    .iter()
                    .filter(|node| planned.contains(&node.stable_id()))
                    .cloned()
                    .collect()
            }
            EnrichmentScope::Explicit(value) => {
                anyhow::bail!("unsupported explicit enrichment scope: {}", value);
            }
        };
        Ok(nodes)
    }

    fn dirty_slugs_for_scope(&self, scope: &EnrichmentScope) -> Option<HashSet<String>> {
        match scope {
            EnrichmentScope::Repo => None,
            EnrichmentScope::Root(root) => Some(std::iter::once(root.clone()).collect()),
            EnrichmentScope::ChangedFiles => {
                let primary_slug = RootConfig::code_project(self.repo_root.clone()).slug();
                Some(std::iter::once(primary_slug).collect())
            }
            EnrichmentScope::TargetSymbols(_) | EnrichmentScope::TaskRelevant { .. } => None,
            EnrichmentScope::Explicit(value) => Some(std::iter::once(value.clone()).collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Confidence, EdgeKind, ExtractionSource, NodeId, NodeKind};
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    fn make_node(name: &str, kind: NodeKind) -> Node {
        make_node_in_file("src/test.rs", name, kind)
    }

    fn make_node_in_file(file: &str, name: &str, kind: NodeKind) -> Node {
        Node {
            id: NodeId {
                root: "test".to_string(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind,
            },
            language: "rust".to_string(),
            line_start: 0,
            line_end: 0,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    #[test]
    fn target_symbol_scope_is_exact_and_rejects_ambiguity() {
        let nodes = vec![
            make_node_in_file("src/one.rs", "Target", NodeKind::Struct),
            make_node_in_file("src/two.rs", "Other", NodeKind::Struct),
        ];
        let target_id = nodes[0].stable_id();
        let planned = plan_explicit_lsp_scope(
            &nodes,
            &EnrichmentScope::TargetSymbols(vec![target_id.clone()]),
        )
        .unwrap();
        assert_eq!(planned.len(), 1);
        assert!(planned.contains(&target_id));
        assert!(!planned.contains(&nodes[1].stable_id()));

        let ambiguous = vec![
            make_node_in_file("src/one.rs", "Target", NodeKind::Struct),
            make_node_in_file("src/two.rs", "Target", NodeKind::Struct),
        ];
        let error = plan_explicit_lsp_scope(
            &ambiguous,
            &EnrichmentScope::TargetSymbols(vec!["Target".to_string()]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn task_relevant_scope_is_union_only_and_rejects_unmapped_files() {
        let nodes = vec![
            make_node_in_file("src/task.rs", "InTask", NodeKind::Struct),
            make_node_in_file("src/target.rs", "NamedTarget", NodeKind::Struct),
            make_node_in_file("src/unrelated.rs", "Unrelated", NodeKind::Struct),
        ];
        let planned = plan_explicit_lsp_scope(
            &nodes,
            &EnrichmentScope::TaskRelevant {
                files: vec!["./src/task.rs".to_string()],
                symbols: vec![nodes[1].stable_id()],
            },
        )
        .unwrap();
        assert_eq!(planned.len(), 2);
        assert!(planned.contains(&nodes[0].stable_id()));
        assert!(planned.contains(&nodes[1].stable_id()));
        assert!(!planned.contains(&nodes[2].stable_id()));

        let error = plan_explicit_lsp_scope(
            &nodes,
            &EnrichmentScope::TaskRelevant {
                files: vec!["src/missing.rs".to_string()],
                symbols: Vec::new(),
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("did not match a cached node"));
    }

    #[test]
    fn pipeline_report_preserves_the_actual_operation_start() {
        let duration = Duration::from_secs(60);
        let report = build_pipeline_operation_report(
            std::path::Path::new("/tmp/repo"),
            PipelineReportInput {
                operation: OperationKind::Scan,
                enrichment: ScanEnrichmentOptions::extract_only(),
                duration,
                symbol_count: 0,
                edge_count: 0,
                file_count: 0,
                lsp_edge_count: 0,
                lsp_state: CapabilityState::Skipped,
                lsp_detail: None,
                embedding_count: 0,
                embeddings_attached: false,
                phases: Vec::new(),
                related_job_ids: Vec::new(),
                business_context: crate::business_context::BusinessContextAdmission::default(),
            },
        );

        assert_eq!(
            report
                .completed_at
                .unwrap()
                .saturating_sub(report.started_at),
            duration.as_secs()
        );
    }

    #[test]
    fn changed_file_scope_never_continues_to_repo_work() {
        assert!(!should_continue_lsp_enrichment(
            &EnrichmentScope::ChangedFiles,
            EnrichmentContinuation::SpawnBackground,
        ));
        assert!(!should_continue_lsp_enrichment(
            &EnrichmentScope::ChangedFiles,
            EnrichmentContinuation::RunToCompletion,
        ));
        assert!(should_continue_lsp_enrichment(
            &EnrichmentScope::Root("fixture".to_string()),
            EnrichmentContinuation::RunToCompletion,
        ));
    }

    #[test]
    fn final_edge_dedup_preserves_lsp_confirmation_over_extraction_duplicate() {
        let caller = make_node("caller", NodeKind::Function);
        let callee = make_node("callee", NodeKind::Function);
        let extracted = Edge {
            from: caller.id.clone(),
            to: callee.id.clone(),
            kind: EdgeKind::Calls,
            source: ExtractionSource::TreeSitter,
            confidence: Confidence::Detected,
            evidence: Vec::new(),
        };
        let mut confirmed = extracted.clone();
        confirmed.source = ExtractionSource::Lsp;
        confirmed.confidence = Confidence::Confirmed;
        let stable_id = confirmed.stable_id();
        let mut edges = vec![extracted, confirmed];

        dedup_edges_preserving_lsp_evidence(&mut edges);

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].stable_id(), stable_id);
        assert_eq!(edges[0].source, ExtractionSource::Lsp);
        assert_eq!(edges[0].confidence, Confidence::Confirmed);
    }

    #[test]
    fn scoped_lsp_persistence_replaces_only_planned_edges() {
        let planned = make_node("planned", NodeKind::Function);
        let unrelated = make_node("unrelated", NodeKind::Function);
        let mut old_target = make_node("old_target", NodeKind::Function);
        old_target.id.root = "external".to_string();
        old_target.source = ExtractionSource::Lsp;
        let mut new_target = make_node("new_target", NodeKind::Function);
        new_target.id.root = "external".to_string();
        new_target.source = ExtractionSource::Lsp;
        let new_non_lsp_node = make_node("framework", NodeKind::Other("framework".to_string()));

        let stale_scoped_edge = Edge {
            from: planned.id.clone(),
            to: old_target.id.clone(),
            kind: EdgeKind::Calls,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let unrelated_edge = Edge {
            from: unrelated.id.clone(),
            to: old_target.id.clone(),
            kind: EdgeKind::Calls,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let fresh_scoped_edge = Edge {
            from: planned.id.clone(),
            to: new_target.id.clone(),
            kind: EdgeKind::Calls,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let new_non_lsp_edge = Edge {
            from: new_non_lsp_node.id.clone(),
            to: planned.id.clone(),
            kind: EdgeKind::DependsOn,
            source: ExtractionSource::TreeSitter,
            confidence: Confidence::Detected,
            evidence: Vec::new(),
        };
        let node_filter = HashSet::from([planned.stable_id()]);
        let mut cached_edges = vec![stale_scoped_edge.clone(), unrelated_edge.clone()];
        let existing_node_ids = [&planned, &unrelated, &old_target]
            .into_iter()
            .map(|node| node.stable_id())
            .collect::<HashSet<_>>();
        let existing_edge_ids = cached_edges
            .iter()
            .map(Edge::stable_id)
            .collect::<HashSet<_>>();

        let deleted_edge_ids = remove_existing_scoped_lsp_edges(&mut cached_edges, &node_filter);
        assert_eq!(cached_edges.len(), 1);
        assert_eq!(cached_edges[0].stable_id(), unrelated_edge.stable_id());

        let delta = scoped_lsp_persistence_delta(
            &[
                planned,
                unrelated,
                old_target,
                new_target.clone(),
                new_non_lsp_node.clone(),
            ],
            &[
                unrelated_edge,
                fresh_scoped_edge.clone(),
                new_non_lsp_edge.clone(),
            ],
            &node_filter,
            &existing_node_ids,
            &existing_edge_ids,
            deleted_edge_ids,
        );
        assert_eq!(delta.deleted_edge_ids, vec![stale_scoped_edge.stable_id()]);
        let upsert_edge_ids = delta
            .upsert_edges
            .iter()
            .map(Edge::stable_id)
            .collect::<HashSet<_>>();
        assert_eq!(
            upsert_edge_ids,
            HashSet::from([fresh_scoped_edge.stable_id(), new_non_lsp_edge.stable_id(),])
        );
        let upsert_node_ids = delta
            .upsert_nodes
            .iter()
            .map(Node::stable_id)
            .collect::<HashSet<_>>();
        assert_eq!(
            upsert_node_ids,
            HashSet::from([new_target.stable_id(), new_non_lsp_node.stable_id()])
        );
    }

    #[test]
    fn incremental_scope_purges_stale_edges_and_only_orphaned_lsp_nodes() {
        let planned = make_node_in_file("src/changed.py", "planned", NodeKind::Function);
        let unrelated = make_node_in_file("src/unchanged.py", "unrelated", NodeKind::Function);
        let mut orphaned = make_node_in_file("<external>", "orphaned", NodeKind::Function);
        orphaned.id.root = "external".to_string();
        orphaned.source = ExtractionSource::Lsp;
        let mut shared = make_node_in_file("<external>", "shared", NodeKind::Function);
        shared.id.root = "external".to_string();
        shared.source = ExtractionSource::Lsp;
        let edge = |from: &Node, to: &Node| Edge {
            from: from.id.clone(),
            to: to.id.clone(),
            kind: EdgeKind::Calls,
            source: ExtractionSource::Lsp,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let stale_orphan_edge = edge(&planned, &orphaned);
        let stale_shared_edge = edge(&planned, &shared);
        let retained_shared_edge = edge(&unrelated, &shared);
        let mut nodes = vec![planned.clone(), unrelated, orphaned.clone(), shared.clone()];
        let mut edges = vec![
            stale_orphan_edge.clone(),
            stale_shared_edge.clone(),
            retained_shared_edge.clone(),
        ];

        let removed = purge_existing_scoped_lsp_output(
            &mut nodes,
            &mut edges,
            &HashSet::from([planned.stable_id()]),
            &HashSet::from([PathBuf::from("src/changed.py")]),
        );

        assert_eq!(
            removed.into_iter().collect::<HashSet<_>>(),
            HashSet::from([stale_orphan_edge.stable_id(), stale_shared_edge.stable_id()])
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].stable_id(), retained_shared_edge.stable_id());
        assert!(
            !nodes
                .iter()
                .any(|node| node.stable_id() == orphaned.stable_id())
        );
        assert!(
            nodes
                .iter()
                .any(|node| node.stable_id() == shared.stable_id())
        );
    }

    #[tokio::test]
    async fn deleted_or_old_rename_document_symbols_cannot_survive_persisted_readiness_purge() {
        let repo = tempfile::tempdir().unwrap();
        let stale_path = PathBuf::from("astropy/io/fits/src/compressionmodule.c");
        let mut stale_document_symbol = make_node_in_file(
            stale_path.to_str().unwrap(),
            "compress@discarded",
            NodeKind::Other("lsp_document_symbol".to_string()),
        );
        stale_document_symbol.source = ExtractionSource::Lsp;
        let module = make_node_in_file(
            "astropy/io/fits/src",
            "astropy.io.fits.src",
            NodeKind::Module,
        );
        let subsystem = make_node_in_file(
            "subsystems/tests",
            "tests",
            NodeKind::Other("subsystem".to_string()),
        );
        let mut live_document_symbol = make_node_in_file(
            "astropy/io/fits/src/live.c",
            "live@retained",
            NodeKind::Other("lsp_document_symbol".to_string()),
        );
        live_document_symbol.source = ExtractionSource::Lsp;
        let edge = |from: &Node, to: &Node, kind: EdgeKind, source: ExtractionSource| Edge {
            from: from.id.clone(),
            to: to.id.clone(),
            kind,
            source,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        };
        let stale_module_edge = edge(
            &stale_document_symbol,
            &module,
            EdgeKind::BelongsTo,
            ExtractionSource::TreeSitter,
        );
        let stale_subsystem_edge = edge(
            &stale_document_symbol,
            &subsystem,
            EdgeKind::BelongsTo,
            ExtractionSource::TreeSitter,
        );
        let stale_cross_file_edge = edge(
            &live_document_symbol,
            &stale_document_symbol,
            EdgeKind::Calls,
            ExtractionSource::Lsp,
        );
        let retained_unrelated_edge = edge(
            &live_document_symbol,
            &module,
            EdgeKind::BelongsTo,
            ExtractionSource::TreeSitter,
        );
        let mut nodes = vec![
            stale_document_symbol.clone(),
            module,
            subsystem,
            live_document_symbol.clone(),
        ];
        let mut edges = vec![
            stale_module_edge.clone(),
            stale_subsystem_edge.clone(),
            stale_cross_file_edge.clone(),
            retained_unrelated_edge.clone(),
        ];
        let stale_paths = BTreeSet::from([stale_path.clone()]);

        persist_graph_to_lance(repo.path(), &nodes, &edges)
            .await
            .expect("persist stale fixture graph");
        let rejected = crate::structural_cache::validate_persisted_target(
            repo.path(),
            &nodes,
            &edges,
            &stale_paths,
        )
        .await
        .expect_err("persisted readiness must reject a stale path");
        assert!(
            rejected
                .to_string()
                .contains("retains a deleted/old-rename path")
        );

        let removed = purge_existing_scoped_lsp_output(
            &mut nodes,
            &mut edges,
            &HashSet::new(),
            &HashSet::from([stale_path.clone()]),
        );

        assert_eq!(
            removed.into_iter().collect::<HashSet<_>>(),
            HashSet::from([
                stale_module_edge.stable_id(),
                stale_subsystem_edge.stable_id(),
                stale_cross_file_edge.stable_id(),
            ])
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].stable_id(), retained_unrelated_edge.stable_id());
        assert!(
            !nodes
                .iter()
                .any(|node| node.stable_id() == stale_document_symbol.stable_id())
        );
        assert!(
            nodes
                .iter()
                .any(|node| node.stable_id() == live_document_symbol.stable_id())
        );
        assert!(nodes.iter().all(|node| node.id.file != stale_path));
        assert!(
            edges
                .iter()
                .all(|edge| edge.from.file != stale_path && edge.to.file != stale_path)
        );

        persist_graph_to_lance(repo.path(), &nodes, &edges)
            .await
            .expect("persist purged fixture graph");
        crate::structural_cache::validate_persisted_target(
            repo.path(),
            &nodes,
            &edges,
            &stale_paths,
        )
        .await
        .expect("purged graph survives reopen and persisted readiness validation");
    }

    #[test]
    fn test_cache_needs_enrichment_empty_graph() {
        assert!(!cache_needs_enrichment(&[]));
    }

    #[test]
    fn test_cache_needs_enrichment_no_imports() {
        let nodes = vec![make_node("Config", NodeKind::Function)];
        assert!(!cache_needs_enrichment(&nodes));
    }

    #[test]
    fn test_cache_needs_enrichment_imports_with_frameworks_but_no_framework_nodes() {
        // Import matches a known framework pattern ("tokio") but no framework
        // nodes exist -- cache is stale.
        let mut import_node = make_node("use tokio::runtime::Runtime", NodeKind::Import);
        import_node.id.file = PathBuf::from("src/main.rs");
        let nodes = vec![import_node];
        assert!(cache_needs_enrichment(&nodes));
    }

    #[test]
    fn test_cache_needs_enrichment_imports_with_framework_nodes_present() {
        // Import matches a known framework AND a framework node exists -- cache is fine.
        let mut import_node = make_node("use tokio::runtime::Runtime", NodeKind::Import);
        import_node.id.file = PathBuf::from("src/main.rs");
        let mut fw_node = make_node("tokio", NodeKind::Other("framework".to_string()));
        fw_node.id.file = PathBuf::from("frameworks/tokio");
        let nodes = vec![import_node, fw_node];
        assert!(!cache_needs_enrichment(&nodes));
    }

    #[test]
    fn test_cache_needs_enrichment_imports_no_matching_framework() {
        // Import exists but doesn't match any known framework rule -- cache is fine.
        let import_node = make_node("use my_unknown_crate::Foo", NodeKind::Import);
        let nodes = vec![import_node];
        assert!(!cache_needs_enrichment(&nodes));
    }

    #[test]
    fn test_lsp_abort_failures_are_scoped_to_participating_slugs() {
        let mut stats = crate::extract::scan_stats::ScanStats::default();
        stats.lsp_stats.insert(
            "current".to_string(),
            [(
                "rust".to_string(),
                crate::extract::scan_stats::LspLanguageStats {
                    server_name: "rust-analyzer".to_string(),
                    edge_count: 0,
                    node_count: 0,
                    duration: Duration::from_secs(1),
                    error_count: 2,
                    aborted: true,
                    server_missing: false,
                    remediation: None,
                    query_metrics: Vec::new(),
                    validation: None,
                },
            )]
            .into_iter()
            .collect(),
        );
        stats.lsp_stats.insert(
            "stale".to_string(),
            [(
                "rust".to_string(),
                crate::extract::scan_stats::LspLanguageStats {
                    server_name: "rust-analyzer".to_string(),
                    edge_count: 0,
                    node_count: 0,
                    duration: Duration::from_secs(1),
                    error_count: 9,
                    aborted: true,
                    server_missing: false,
                    remediation: None,
                    query_metrics: Vec::new(),
                    validation: None,
                },
            )]
            .into_iter()
            .collect(),
        );

        let participating_slugs = HashSet::from(["current".to_string()]);
        let failures = lsp_abort_failures_for_slugs(&stats, &participating_slugs);

        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("current/rust"));
        assert!(!failures[0].contains("stale/rust"));
    }

    #[tokio::test]
    async fn validation_evidence_is_invocation_local() {
        let temp = tempfile::TempDir::new().unwrap();
        let scan_stats = Arc::new(std::sync::RwLock::new(
            crate::extract::scan_stats::ScanStats::default(),
        ));
        scan_stats.write().unwrap().lsp_stats.insert(
            "stale".to_string(),
            [(
                "rust".to_string(),
                crate::extract::scan_stats::LspLanguageStats {
                    server_name: "rust-analyzer".to_string(),
                    edge_count: 0,
                    node_count: 0,
                    duration: Duration::from_secs(1),
                    error_count: 0,
                    aborted: false,
                    server_missing: false,
                    remediation: None,
                    query_metrics: Vec::new(),
                    validation: Some(
                        crate::extract::scan_stats::LspValidationEvidence::processed(
                            "rust",
                            "rust-analyzer",
                            "workspace/symbol",
                            9,
                        ),
                    ),
                },
            )]
            .into_iter()
            .collect(),
        );

        let (_, _, _, _, validations) = emit_lsp_pipeline_with_budget(LspPipelineInput {
            nodes: Vec::new(),
            edges: Vec::new(),
            root_pairs: vec![("current".to_string(), temp.path().to_path_buf())],
            primary_slug: "current".to_string(),
            repo_root: temp.path().to_path_buf(),
            scan_stats,
            skip_lsp: true,
            dirty_slugs: None,
            lsp_node_filter: None,
            file_readiness_filter: None,
            broad_reference_budget: None,
        })
        .await
        .expect("empty invocation should finalize");

        assert!(
            validations.is_empty(),
            "current job must not inherit readiness evidence from cumulative scan stats"
        );
    }
}
