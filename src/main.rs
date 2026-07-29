use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::ToMcpServerHandler;
use rust_mcp_sdk::schema::{Implementation, InitializeResult, ServerCapabilities};

use repo_native_alignment::adr::{self, ValidateSelection};
use repo_native_alignment::business_context::{BusinessContextAdmission, BusinessContextMode};
use repo_native_alignment::roots::WorkspaceConfig;
use repo_native_alignment::server::{
    self, BroadReferenceBudget, EnrichmentCapability, EnrichmentContinuation, EnrichmentJobLedger,
    EnrichmentJobState, EnrichmentScope, RnaHandler, ScanEnrichmentOptions,
};
use repo_native_alignment::service::{
    self, GraphParams, OutcomeProgressContext, OutcomeProgressParams, RepoMapContext,
    RepoMapParams, SearchContext, SearchParams,
};
use repo_native_alignment::setup::{self, SetupArgs};
use repo_native_alignment::smoke_test::{self, TestArgs};

#[derive(Parser, Debug)]
#[command(name = "repo-native-alignment", version, about = "Repo-Native Alignment MCP Server", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long, default_value = "stdio")]
    transport: String,
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 8382)]
    port: u16,
    #[arg(long)]
    log_path: Option<PathBuf>,
    /// Serve only an existing admitted cache; never scan, enrich, download, or mutate it.
    #[arg(long)]
    cache_only: bool,
    /// Include RNA-specific `.oh` and Git-history context in produced indexes.
    #[arg(long, default_value_t = BusinessContextMode::Enabled)]
    business_context: BusinessContextMode,
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    Setup(SetupArgs),
    Test(TestArgs),
    Scan(ScanArgs),
    /// Verify persisted per-file LSP completeness before benchmark/model access.
    LspReadiness(LspReadinessArgs),
    /// Print the exact producer/schema and target-tree identity used for
    /// verifier-clean structural-cache reuse.
    StructuralCacheIdentity(StructuralCacheIdentityArgs),
    /// Replay a retained post-LSP failure cache without scanning or LSP calls.
    #[command(hide = true)]
    StructuralCacheReplay(StructuralCacheReplayArgs),
    Enrich(EnrichArgs),
    Search(SearchArgs),
    Graph(GraphArgs),
    Stats(StatsArgs),
    /// Track progress on a business outcome
    OutcomeProgress(OutcomeProgressCli),
    /// List configured workspace roots
    ListRoots(ListRootsCli),
    /// Show a high-level repository map
    RepoMap(RepoMapCli),
    /// Compile or validate ADR executable declarations
    Adr(AdrArgs),
    /// Open an interactive graph visualizer in the browser
    Open(OpenArgs),
}

#[derive(clap::Args, Debug)]
struct OpenArgs {
    /// Path to the repo to visualize (must have been scanned first)
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}

#[derive(clap::Args, Debug)]
struct StatsArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}
#[derive(clap::Args, Debug)]
struct ScanArgs {
    #[arg(long)]
    path: Option<PathBuf>,
    #[arg(long)]
    full: bool,
    /// Run extraction only; skip LSP and embedding enrichment.
    #[arg(long)]
    extract_only: bool,
    /// Skip LSP call/reference enrichment for this scan.
    #[arg(long)]
    no_lsp: bool,
    /// Skip embedding/semantic-index enrichment for this scan.
    #[arg(long)]
    no_embed: bool,
    /// Print measured operation phase timings after the scan summary.
    #[arg(long)]
    timings: bool,
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}

#[derive(clap::Args, Debug)]
struct LspReadinessArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Emit the complete machine-readable report and compatibility result.
    #[arg(long)]
    json: bool,
    /// Frozen N=70 case manifest. Each case binds instance/repository/base commit to a report.
    #[arg(long)]
    cohort_manifest: Option<PathBuf>,
    /// Destination for the deterministic aggregate manifest.
    #[arg(long, requires = "cohort_manifest")]
    aggregate_output: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct StructuralCacheIdentityArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}

#[derive(clap::Args, Debug)]
struct StructuralCacheReplayArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    failure_receipt: PathBuf,
    #[arg(long)]
    failure_receipt_sha256: String,
    #[arg(long)]
    authorization_sha256: String,
    #[arg(long)]
    toolchain_lock_digest: String,
    #[arg(long)]
    inventory_digest: String,
    #[arg(long)]
    inventory_file_sha256: String,
    #[arg(long)]
    configuration_digest: String,
    #[arg(long)]
    repository: String,
    #[arg(long)]
    root_slug: String,
    #[arg(long)]
    target_commit: String,
    #[arg(long)]
    target_tree: String,
    /// New path for the non-publishable diagnostic receipt.
    #[arg(long)]
    output: PathBuf,
}

#[derive(clap::Args, Debug)]
struct EnrichArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long, value_enum)]
    capability: EnrichCapabilityArg,
    #[arg(long, value_enum, default_value_t = EnrichScopeArg::Repo)]
    scope: EnrichScopeArg,
    /// Workspace root slug to enrich when `--scope root` is selected.
    #[arg(long)]
    root: Option<String>,
    /// Target stable node ID or unique symbol name. Repeat for multiple targets.
    #[arg(long = "target-symbol")]
    target_symbols: Vec<String>,
    /// Task-relevant repository-relative file. Repeat for multiple files.
    #[arg(long = "task-file")]
    task_files: Vec<String>,
    /// Maximum broad-reference requests across all language servers.
    #[arg(long, default_value_t = 512)]
    max_requests: usize,
    /// Maximum elapsed time for the scoped broad-reference request.
    #[arg(long, default_value_t = 120_000)]
    max_duration_ms: u64,
    /// Do not continue repo-wide enrichment after the requested scope completes.
    #[arg(long)]
    no_background_continuation: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EnrichCapabilityArg {
    Embeddings,
    CallReferences,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EnrichScopeArg {
    Repo,
    Root,
    Changed,
    Targets,
    Task,
}
#[derive(clap::Args, Debug)]
struct SearchArgs {
    #[arg(default_value = "")]
    query: String,
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long)]
    kind: Option<String>,
    #[arg(long)]
    language: Option<String>,
    #[arg(long)]
    file: Option<String>,
    /// First 1-based source line. Also accepts --file path:line:column.
    #[arg(long)]
    line: Option<u32>,
    /// Last 1-based source line (inclusive, maximum span: 200 lines).
    #[arg(long)]
    end_line: Option<u32>,
    #[arg(long, default_value_t = 20)]
    limit: usize,
    #[arg(long)]
    node: Option<String>,
    #[arg(long)]
    mode: Option<String>,
    #[arg(long)]
    hops: Option<u32>,
    #[arg(long)]
    depth: Option<u32>,
    #[arg(long)]
    direction: Option<String>,
    #[arg(long)]
    edge_types: Option<String>,
    #[arg(long)]
    sort_by: Option<String>,
    #[arg(long)]
    min_complexity: Option<u32>,
    #[arg(long)]
    synthetic: Option<bool>,
    #[arg(long)]
    compact: bool,
    #[arg(long)]
    nodes: Option<String>,
    #[arg(long)]
    search_mode: Option<String>,
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true"
    )]
    include_artifacts: bool,
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "true"
    )]
    include_markdown: bool,
    #[arg(long)]
    artifact_types: Option<String>,
    #[arg(long)]
    root: Option<String>,
    #[arg(long)]
    rerank: bool,
    #[arg(long)]
    subsystem: Option<String>,
    #[arg(long)]
    target_subsystem: Option<String>,
    #[arg(long)]
    include_body: bool,
    #[arg(long)]
    minify_body: bool,
    /// Show index stats (default: true for CLI, false for MCP)
    #[arg(long, default_value_t = true)]
    verbose: bool,
    /// Output projection: agent (default) or evidence.
    #[arg(long)]
    projection: Option<String>,
    /// Body policy: complete, focused_span, signature_only, minified, or none.
    #[arg(long)]
    body_policy: Option<String>,
    /// Maximum final rendered UTF-8 bytes.
    #[arg(long)]
    max_output_bytes: Option<usize>,
    /// Maximum estimated rendered tokens (not provider usage).
    #[arg(long)]
    max_output_tokens: Option<usize>,
    /// Maximum source-body UTF-8 bytes per selected record.
    #[arg(long)]
    max_body_bytes: Option<usize>,
    /// Maximum source-body UTF-8 bytes across all records after coalescing.
    #[arg(long)]
    max_total_body_bytes: Option<usize>,
    /// Opt-in context mode: task or graph-delta-beta.
    #[arg(long)]
    context_mode: Option<String>,
    /// Comma-separated task roles: editable_source, definition_or_api_state,
    /// test, behavioral_analogue, direct_dependency, caller_or_impact,
    /// proposal_delta.
    #[arg(long)]
    context_roles: Option<String>,
    /// Comma-separated task facets: behavior, api_or_state, test, analogue,
    /// proposal.
    #[arg(long)]
    context_facets: Option<String>,
    /// Unified diff or structured edit sketch for graph-delta beta.
    #[arg(long)]
    proposal: Option<String>,
}
#[derive(clap::Args, Debug)]
struct GraphArgs {
    #[arg(long)]
    node: String,
    #[arg(long, default_value = "neighbors")]
    mode: String,
    #[arg(long, default_value = "outgoing")]
    direction: String,
    #[arg(long)]
    edge_types: Option<String>,
    #[arg(long)]
    max_hops: Option<usize>,
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}
#[derive(clap::Args, Debug)]
struct OutcomeProgressCli {
    outcome_id: String,
    #[arg(long)]
    include_impact: bool,
    #[arg(long)]
    root: Option<String>,
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}
#[derive(clap::Args, Debug)]
struct ListRootsCli {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}
#[derive(clap::Args, Debug)]
struct RepoMapCli {
    #[arg(long, default_value_t = 15)]
    top_n: usize,
    #[arg(long)]
    root: Option<String>,
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}

#[derive(clap::Args, Debug)]
struct AdrArgs {
    #[command(subcommand)]
    command: AdrCommand,
}

#[derive(Subcommand, Debug)]
enum AdrCommand {
    /// Compile ADR frontmatter into .oh/adr-validation manifests
    Compile(AdrCompileArgs),
    /// Execute compiled ADR validations and enforce status gating
    Validate(AdrValidateArgs),
    /// Run one built-in ADR audit by name
    Audit(AdrAuditArgs),
}

#[derive(clap::Args, Debug)]
struct AdrCompileArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long, default_value = "docs/ADRs")]
    dir: PathBuf,
    #[arg(long)]
    check: bool,
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args, Debug)]
struct AdrValidateArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long)]
    id: Option<String>,
    #[arg(long)]
    path: Option<PathBuf>,
    #[arg(long = "cargo-arg", allow_hyphen_values = true)]
    cargo_args: Vec<String>,
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args, Debug)]
struct AdrAuditArgs {
    name: String,
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long)]
    json: bool,
}

fn server_details() -> InitializeResult {
    InitializeResult {
        capabilities: ServerCapabilities {
            tools: Some(Default::default()),
            ..Default::default()
        },
        instructions: Some(
            "Repo-Native Alignment: query business outcomes, code, markdown, and git history."
                .into(),
        ),
        meta: None,
        protocol_version: "2025-11-25".into(),
        server_info: Implementation {
            name: "rna-server".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: Some(
                "MCP server for querying business outcomes, code, and git history".into(),
            ),
            icons: vec![],
            title: Some("Repo-Native Alignment".into()),
            website_url: None,
        },
    }
}

/// Baseline directives that suppress noisy library internals.
/// These are prepended to whatever default_filter the caller provides,
/// so caller-level directives (e.g. `info`) win for RNA's own crate while
/// lance internals stay at WARN unless RUST_LOG explicitly overrides.
const LIBRARY_NOISE_FILTER: &str = "lance=warn,lance_index=warn,lance_file=warn,lancedb=warn";

fn init_tracing(default_filter: &str, log_path: Option<&std::path::Path>) {
    use tracing_subscriber::prelude::*;
    let composite_default = format!("{},{}", LIBRARY_NOISE_FILTER, default_filter);
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| composite_default.into());
    let effective_log_path = log_path
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::var("RNA_LOG_FILE").ok().map(PathBuf::from));
    if let Some(ref file_path) = effective_log_path {
        if let Some(parent) = file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file_path)
        {
            let file_layer = tracing_subscriber::fmt::layer()
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false);
            let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
            tracing_subscriber::registry()
                .with(env_filter)
                .with(stderr_layer)
                .with(file_layer)
                .init();
            tracing::info!("Logging to file: {}", file_path.display());
            return;
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Load graph from LanceDB cache or exit with instructions to scan first.
async fn try_load_cached_graph(
    repo_root: &std::path::Path,
) -> anyhow::Result<Option<server::state::GraphState>> {
    let lance_path = repo_root.join(".oh").join(".cache").join("lance");
    if !lance_path.exists() {
        return Ok(None);
    }

    if !lance_path.join("symbols.lance").exists() || !lance_path.join("edges.lance").exists() {
        return Ok(None);
    }

    match server::load_graph_from_lance(repo_root).await {
        Ok(state) => {
            eprintln!("Loaded {} symbols from cache.", state.nodes.len());
            Ok(Some(state))
        }
        Err(e) => Err(e.context("Failed to load cached index")),
    }
}

/// Restore process-local LSP readiness from the durable job ledger.
///
/// Cache-only and incremental CLI scans run in a fresh process, so the live atomic
/// status starts empty even when the previous process durably recorded a degraded
/// or completed call-reference job. The newest non-superseded ledger record is the
/// authoritative readiness state for these summaries.
fn hydrate_lsp_status_from_ledger(
    handler: &RnaHandler,
    repo_root: &std::path::Path,
    persisted_lsp_edges: usize,
    hydrate_probe_unavailable: bool,
) -> Vec<String> {
    let jobs = handler.enrichment_jobs.recent_jobs(repo_root, 100);
    let Some(job) = jobs
        .iter()
        .filter(|job| {
            job.capability == EnrichmentCapability::CallReferences
                && !matches!(
                    job.state,
                    EnrichmentJobState::Cancelled | EnrichmentJobState::Superseded
                )
        })
        .max_by_key(|job| (job.updated_at, job.revision))
    else {
        return Vec::new();
    };

    let should_hydrate = job.lsp_evidence.is_some()
        || matches!(
            handler.lsp_status.current_state(),
            server::LspState::NotStarted | server::LspState::ServerFound
        )
        || (hydrate_probe_unavailable
            && handler.lsp_status.current_state() == server::LspState::Unavailable);
    if should_hydrate {
        if let Some(evidence) = job.lsp_evidence.as_ref() {
            match evidence.readiness {
                server::LspEvidenceReadiness::DefaultProfile => {
                    handler.lsp_status.set_complete_default_profile(
                        job.counters.edge_count.unwrap_or(0),
                        persisted_lsp_edges,
                        evidence
                            .detail
                            .clone()
                            .unwrap_or_else(|| "broad references were omitted".to_string()),
                    );
                }
                server::LspEvidenceReadiness::Full => {
                    handler.lsp_status.set_complete(persisted_lsp_edges);
                }
                server::LspEvidenceReadiness::Scoped => {
                    handler.lsp_status.set_complete_scoped(
                        job.counters.edge_count.unwrap_or(0),
                        persisted_lsp_edges,
                        evidence.scope.clone(),
                    );
                }
                server::LspEvidenceReadiness::Partial => {
                    let detail = evidence
                        .detail
                        .as_deref()
                        .or(job.failure.as_deref())
                        .unwrap_or("call-reference enrichment finalized with partial evidence");
                    if job.scope == EnrichmentScope::Repo {
                        handler.lsp_status.set_degraded_with_coverage(
                            job.counters.edge_count.unwrap_or(0),
                            persisted_lsp_edges,
                            detail,
                        );
                    } else {
                        handler.lsp_status.set_degraded_scoped(
                            job.counters.edge_count.unwrap_or(0),
                            persisted_lsp_edges,
                            evidence.scope.clone(),
                            detail,
                        );
                    }
                }
                server::LspEvidenceReadiness::Unavailable => {
                    handler.lsp_status.set_unavailable_with_detail(
                        evidence
                            .detail
                            .as_deref()
                            .or(job.failure.as_deref())
                            .unwrap_or("call-reference evidence unavailable"),
                    );
                }
            }
            return vec![job.job_id.clone()];
        }
        match job.state {
            EnrichmentJobState::Completed if job.scope == EnrichmentScope::Repo => {
                handler.lsp_status.set_complete(persisted_lsp_edges);
            }
            EnrichmentJobState::Completed => handler.lsp_status.set_complete_scoped(
                job.counters.edge_count.unwrap_or(0),
                persisted_lsp_edges,
                format!("{} scope", job.scope.stable_key()),
            ),
            EnrichmentJobState::Degraded => handler.lsp_status.set_degraded(
                persisted_lsp_edges,
                job.failure
                    .as_deref()
                    .unwrap_or("call-reference enrichment finalized with degraded output"),
            ),
            EnrichmentJobState::Failed => handler.lsp_status.set_failed(
                job.failure
                    .as_deref()
                    .unwrap_or("call-reference enrichment failed"),
            ),
            EnrichmentJobState::Queued
            | EnrichmentJobState::Running
            | EnrichmentJobState::Persisting => handler.lsp_status.set_running(),
            EnrichmentJobState::Cancelled | EnrichmentJobState::Superseded => unreachable!(),
        }
    }

    vec![job.job_id.clone()]
}

async fn load_existing_embedding_index(
    repo_root: &std::path::Path,
    offline: bool,
    warn: impl FnOnce(String),
) -> Option<repo_native_alignment::embed::EmbeddingIndex> {
    let opened = if offline {
        repo_native_alignment::embed::EmbeddingIndex::open_existing_offline(repo_root).await
    } else {
        repo_native_alignment::embed::EmbeddingIndex::open_existing(repo_root).await
    };
    match opened {
        Ok(Some(idx)) => match idx.has_table().await {
            Ok(true) => Some(idx),
            Ok(false) => None,
            Err(e) => {
                warn(format!("Embedding index check failed: {}", e));
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            warn(format!("EmbeddingIndex init failed: {}", e));
            None
        }
    }
}

struct ScanSummaryInput<'a> {
    repo_root: &'a std::path::Path,
    graph: &'a server::state::GraphState,
    embeddings_loaded: bool,
    elapsed: std::time::Duration,
    enrichment: ScanEnrichmentOptions,
    timings: bool,
    operation: server::operation_report::OperationKind,
    extra_phases: Vec<server::operation_report::PhaseReport>,
    lsp_state: server::operation_report::CapabilityState,
    lsp_detail: Option<String>,
    related_job_ids: Vec<String>,
    business_context: &'a repo_native_alignment::business_context::BusinessContextAdmission,
}

fn lsp_call_edge_count(graph: &server::state::GraphState) -> usize {
    graph
        .edges
        .iter()
        .filter(|edge| {
            edge.source == repo_native_alignment::graph::ExtractionSource::Lsp
                && matches!(edge.kind, repo_native_alignment::graph::EdgeKind::Calls)
        })
        .count()
}

fn print_scan_summary(input: ScanSummaryInput<'_>) {
    let ScanSummaryInput {
        repo_root,
        graph,
        embeddings_loaded,
        elapsed,
        enrichment,
        timings,
        operation,
        extra_phases,
        lsp_state,
        lsp_detail,
        related_job_ids,
        business_context,
    } = input;
    let mut report = server::operation_report::OperationReport::new(
        operation,
        server::operation_report::OperationTrigger::Cli,
        repo_root,
    )
    .with_scope("repo")
    .complete(elapsed);
    report.outputs = server::operation_report::OutputReport {
        symbol_count: Some(graph.nodes.len()),
        edge_count: Some(graph.edges.len()),
        file_count: None,
        embedding_count: None,
        lsp_edge_count: Some(lsp_call_edge_count(graph)),
    };
    report.related_job_ids = related_job_ids;
    report.record_business_context(business_context);
    for phase in extra_phases {
        report.add_phase(phase);
    }
    report.add_phase(server::operation_report::PhaseReport::ran(
        server::operation_report::PhaseKind::Total,
        elapsed,
    ));
    let (embedding_state, embedding_detail) =
        server::operation_report::embedding_capability_from_availability(
            enrichment,
            embeddings_loaded,
        );
    for capability in server::operation_report::scan_capability_reports(
        enrichment,
        embedding_state,
        embedding_detail,
        lsp_state,
        lsp_detail,
        Some("repo".to_string()),
    ) {
        report.add_capability(capability);
    }
    server::operation_report::add_scan_degradation_and_next_steps(
        &mut report,
        repo_root,
        enrichment,
        embedding_state,
        lsp_state,
    );
    if !enrichment.runs_lsp() {
        report.add_phase(server::operation_report::PhaseReport::skipped(
            server::operation_report::PhaseKind::Lsp,
            "skipped by scan options",
        ));
    }
    if !enrichment.runs_embeddings() {
        report.add_phase(server::operation_report::PhaseReport::skipped(
            server::operation_report::PhaseKind::Embeddings,
            "skipped by scan options",
        ));
    }
    if let Err(err) = server::OperationReportStore::record(repo_root, report.clone()) {
        tracing::warn!("failed to persist operation report: {err:#}");
    }
    eprintln!();
    eprintln!("{}", report.render_cli(timings));
}

/// Load graph from LanceDB cache or exit with instructions to scan first.
async fn load_cached_graph(repo_root: &std::path::Path) -> server::state::GraphState {
    match try_load_cached_graph(repo_root).await {
        Ok(Some(state)) => state,
        Ok(None) => {
            eprintln!("No index found. Run `repo-native-alignment scan --path .` first.");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{:#}. Run `repo-native-alignment scan --path .` first.", e);
            std::process::exit(1);
        }
    }
}

/// Validate the disposable cache identity before a direct CLI query reads it.
/// Incompatible caches are rebuilt once through the same producer-admission path
/// used by scans; compatible caches keep the existing fast load path.
async fn load_cache_backed_query_graph(
    repo_root: &std::path::Path,
    business_context_mode: BusinessContextMode,
) -> anyhow::Result<server::state::GraphState> {
    let handler = RnaHandler {
        repo_root: repo_root.to_path_buf(),
        business_context: BusinessContextAdmission::new(business_context_mode),
        ..RnaHandler::default()
    };
    if handler
        .prepare_business_context_cache()?
        .requires_fresh_graph()
    {
        let graph = handler
            .build_full_graph_inner(false, ScanEnrichmentOptions::extract_only())
            .await?;
        handler.persist_graph_snapshot(&graph).await?;
        return Ok(graph);
    }

    Ok(load_cached_graph(repo_root).await)
}

fn resolve_root_filter(root_arg: Option<&str>, repo_root: &std::path::Path) -> Option<String> {
    let root_slug =
        repo_native_alignment::roots::RootConfig::code_project(repo_root.to_path_buf()).slug();
    root_arg
        .map(|v| {
            if v.eq_ignore_ascii_case("all") {
                None
            } else {
                Some(v.to_string())
            }
        })
        .unwrap_or_else(|| Some(root_slug))
}

fn main() {
    // Set fastembed model cache to ~/.cache/rna/models/ instead of .fastembed_cache/
    // in the current directory. Must be set before Tokio runtime and any fastembed
    // initialization (reranker model, or any future fastembed embedding model).
    if std::env::var("FASTEMBED_CACHE_DIR")
        .ok()
        .filter(|v| !v.is_empty())
        .is_none()
        && let Ok(home) = std::env::var("HOME")
    {
        let cache_dir = std::path::PathBuf::from(home)
            .join(".cache")
            .join("rna")
            .join("models");
        // SAFETY: called in single-threaded main() before Tokio runtime starts.
        unsafe { std::env::set_var("FASTEMBED_CACHE_DIR", &cache_dir) };
    }

    if let Err(err) = async_main() {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}

#[tokio::main]
async fn async_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let log_path = cli.log_path.clone();
    let business_context_mode = cli.business_context;
    match cli.command {
        Some(Commands::Setup(args)) => return setup::run(&args),
        Some(Commands::Test(args)) => {
            init_tracing("info", log_path.as_deref());
            let passed = smoke_test::run(&args).await?;
            std::process::exit(if passed { 0 } else { 1 });
        }
        Some(Commands::StructuralCacheIdentity(args)) => {
            let repo_root = args.repo.canonicalize()?;
            let identity = repo_native_alignment::structural_cache::current_identity(
                &repo_root,
                business_context_mode,
            )?;
            println!("{}", serde_json::to_string_pretty(&identity)?);
            return Ok(());
        }
        Some(Commands::StructuralCacheReplay(args)) => {
            init_tracing("warn", log_path.as_deref());
            anyhow::ensure!(
                business_context_mode.is_disabled(),
                "structural-cache replay requires --business-context disabled"
            );
            let repo_root = args.repo.canonicalize()?;
            let output_parent = args
                .output
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| std::path::Path::new("."))
                .canonicalize()?;
            anyhow::ensure!(
                !output_parent.starts_with(repo_root.join(".oh/.cache")),
                "diagnostic replay receipt must be written outside the copied cache"
            );
            let receipt =
                repo_native_alignment::structural_cache_replay::replay_retained_structural_cache(
                    &repo_native_alignment::structural_cache_replay::StructuralCacheReplayRequest {
                        repo_root: &repo_root,
                        failure_receipt: &args.failure_receipt,
                        failure_receipt_sha256: &args.failure_receipt_sha256,
                        authorization_sha256: &args.authorization_sha256,
                        toolchain_lock_digest: &args.toolchain_lock_digest,
                        inventory_digest: &args.inventory_digest,
                        inventory_file_sha256: &args.inventory_file_sha256,
                        configuration_digest: &args.configuration_digest,
                        repository: &args.repository,
                        root_slug: &args.root_slug,
                        target_commit: &args.target_commit,
                        target_tree: &args.target_tree,
                    },
                )
                .await?;
            let mut bytes = serde_json::to_vec_pretty(&receipt)?;
            bytes.push(b'\n');
            let mut output = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&args.output)?;
            output.write_all(&bytes)?;
            output.sync_all()?;
            print!("{}", String::from_utf8_lossy(&bytes));
            anyhow::ensure!(
                receipt.diagnostic_checkpoint_validation_passed && receipt.full_target_ready,
                "diagnostic retained-cache replay is not READY"
            );
            return Ok(());
        }
        Some(Commands::Scan(args)) => {
            init_tracing("info", log_path.as_deref());
            let repo_root = args
                .path
                .unwrap_or_else(|| args.repo.clone())
                .canonicalize()?;
            let lsp_only_roots_scan = WorkspaceConfig::load()
                .with_primary_root(repo_root.clone())
                .with_declared_roots(&repo_root)
                .lsp_only_roots();
            let mut enrichment = if args.extract_only {
                ScanEnrichmentOptions::extract_only()
            } else {
                ScanEnrichmentOptions::all()
            };
            if args.no_lsp {
                enrichment = enrichment.without_lsp();
            }
            if args.no_embed {
                enrichment = enrichment.without_embeddings();
            }
            if args.full {
                eprintln!("Full pipeline scan: {}", repo_root.display());
                let handler = RnaHandler {
                    repo_root: repo_root.clone(),
                    business_context: BusinessContextAdmission::new(business_context_mode),
                    lsp_only_roots: Arc::new(lsp_only_roots_scan),
                    ..Default::default()
                };
                let mut result = handler
                    .run_pipeline_foreground(
                        |msg| {
                            eprintln!("{}", msg);
                        },
                        enrichment,
                    )
                    .await?;
                server::OperationReportStore::hydrate_lsp_work_item_evidence(
                    &repo_root,
                    &mut result.report,
                );
                if let Err(err) =
                    server::OperationReportStore::record(&repo_root, result.report.clone())
                {
                    tracing::warn!("failed to persist operation report: {err:#}");
                }
                if args.timings {
                    eprintln!();
                    eprintln!("{}", result.report.render_cli(true));
                }
                if business_context_mode.is_disabled() {
                    let graph_snapshot = handler.graph.load_full();
                    let graph = graph_snapshot.as_ref().as_ref().ok_or_else(|| {
                        anyhow::anyhow!("full scan completed without a graph snapshot")
                    })?;
                    let readiness =
                        repo_native_alignment::lsp_completeness::load_readiness_check_with_graph(
                            &repo_root,
                            business_context_mode,
                            &graph.nodes,
                            &graph.edges,
                        )?;
                    let completeness = &readiness.report;
                    eprintln!(
                        "LSP completeness: {} included file(s), {} per-file violation(s), {} compatibility violation(s), digest={}",
                        completeness.summary.included_files,
                        completeness.violations.len(),
                        readiness.compatibility_violations.len(),
                        completeness.digest,
                    );
                    const MAX_COMPATIBILITY_DIAGNOSTICS: usize = 5;
                    for (index, violation) in readiness
                        .compatibility_violations
                        .iter()
                        .take(MAX_COMPATIBILITY_DIAGNOSTICS)
                        .enumerate()
                    {
                        eprintln!(
                            "  compatibility violation {}: {:?}: {}",
                            index + 1,
                            violation.code,
                            violation.detail,
                        );
                    }
                    if readiness.compatibility_violations.len() > MAX_COMPATIBILITY_DIAGNOSTICS {
                        eprintln!(
                            "  ... {} additional compatibility violation(s) omitted",
                            readiness.compatibility_violations.len()
                                - MAX_COMPATIBILITY_DIAGNOSTICS,
                        );
                    }
                    if !readiness.ready {
                        anyhow::bail!(
                            "benchmark LSP completeness blocked by {} per-file and {} compatibility violation(s); inspect the captured benchmark scan-log artifact above for compatibility details and {} for persisted per-file details",
                            completeness.violations.len(),
                            readiness.compatibility_violations.len(),
                            repo_native_alignment::lsp_completeness::report_path(&repo_root)
                                .display(),
                        );
                    }
                }
                if let Some(degraded) = result.report.capabilities.iter().find(|capability| {
                    capability.capability == EnrichmentCapability::CallReferences
                        && capability.requested
                        && capability.state == server::operation_report::CapabilityState::Degraded
                }) {
                    anyhow::bail!(
                        "LSP call-reference enrichment finalized with degraded output: {}",
                        degraded
                            .detail
                            .as_deref()
                            .unwrap_or("degraded without an actionable diagnostic")
                    );
                }
                return Ok(());
            }
            eprintln!("Scanning: {}", repo_root.display());
            let t0 = std::time::Instant::now();
            let handler = RnaHandler {
                repo_root: repo_root.clone(),
                business_context: BusinessContextAdmission::new(business_context_mode),
                lsp_only_roots: Arc::new(lsp_only_roots_scan),
                ..Default::default()
            };
            handler.prepare_business_context_cache()?;
            if let Some(embed_idx) = load_existing_embedding_index(&repo_root, false, |msg| {
                if enrichment.runs_embeddings() {
                    tracing::warn!("{}; scan summary may show embeddings unavailable", msg);
                } else {
                    tracing::debug!("{}; no embedding enrichment requested", msg);
                }
            })
            .await
            {
                handler.embed_index.store(Arc::new(Some(embed_idx)));
            }

            let mut scanner = repo_native_alignment::scanner::Scanner::new(repo_root.clone())?;
            let scan = scanner.scan()?;
            let scan_duration = scan.scan_duration;
            if scan.changed_files.is_empty()
                && scan.new_files.is_empty()
                && scan.deleted_files.is_empty()
            {
                let (graph, operation) = match try_load_cached_graph(&repo_root).await? {
                    Some(mut graph) => {
                        if handler.refresh_manifest_graph(&mut graph).await? {
                            eprintln!("Refreshed manifest dependency graph.");
                        }
                        scanner.commit_state()?;
                        (graph, server::operation_report::OperationKind::CacheLoad)
                    }
                    None => {
                        let graph = handler.build_full_graph_inner(true, enrichment).await?;
                        scanner.commit_state()?;
                        if enrichment.runs_embeddings() {
                            handler.await_background_embed().await;
                        }
                        (
                            graph,
                            if args.extract_only {
                                server::operation_report::OperationKind::ExtractOnly
                            } else {
                                server::operation_report::OperationKind::Scan
                            },
                        )
                    }
                };
                let elapsed = t0.elapsed();
                let lsp_edge_count = lsp_call_edge_count(&graph);
                let related_job_ids = hydrate_lsp_status_from_ledger(
                    &handler,
                    &repo_root,
                    lsp_edge_count,
                    operation == server::operation_report::OperationKind::CacheLoad,
                );
                let (lsp_state, lsp_detail) = server::operation_report::lsp_capability_from_status(
                    enrichment,
                    handler.lsp_status.current_state(),
                    handler.lsp_status.diagnostic().as_deref(),
                    lsp_edge_count,
                    !related_job_ids.is_empty(),
                );
                print_scan_summary(ScanSummaryInput {
                    repo_root: &repo_root,
                    graph: &graph,
                    embeddings_loaded: handler.embed_index.load().is_some(),
                    elapsed,
                    enrichment,
                    timings: args.timings,
                    operation,
                    extra_phases: vec![server::operation_report::PhaseReport::ran(
                        server::operation_report::PhaseKind::DiscoverFiles,
                        scan_duration,
                    )],
                    lsp_state,
                    lsp_detail,
                    related_job_ids,
                    business_context: &handler.business_context,
                });
                return Ok(());
            }

            let mut graph = match try_load_cached_graph(&repo_root).await? {
                Some(graph) => graph,
                None => {
                    eprintln!("No cached index found; building initial extracted graph.");
                    let graph = handler.build_full_graph_inner(true, enrichment).await?;
                    scanner.commit_state()?;
                    if enrichment.runs_embeddings() {
                        handler.await_background_embed().await;
                    }
                    let elapsed = t0.elapsed();
                    let lsp_edge_count = lsp_call_edge_count(&graph);
                    let related_job_ids =
                        hydrate_lsp_status_from_ledger(&handler, &repo_root, lsp_edge_count, false);
                    let (lsp_state, lsp_detail) =
                        server::operation_report::lsp_capability_from_status(
                            enrichment,
                            handler.lsp_status.current_state(),
                            handler.lsp_status.diagnostic().as_deref(),
                            lsp_edge_count,
                            !related_job_ids.is_empty(),
                        );
                    print_scan_summary(ScanSummaryInput {
                        repo_root: &repo_root,
                        graph: &graph,
                        embeddings_loaded: handler.embed_index.load().is_some(),
                        elapsed,
                        enrichment,
                        timings: args.timings,
                        operation: if args.extract_only {
                            server::operation_report::OperationKind::ExtractOnly
                        } else {
                            server::operation_report::OperationKind::Scan
                        },
                        extra_phases: vec![server::operation_report::PhaseReport::ran(
                            server::operation_report::PhaseKind::DiscoverFiles,
                            scan_duration,
                        )],
                        lsp_state,
                        lsp_detail,
                        related_job_ids,
                        business_context: &handler.business_context,
                    });
                    return Ok(());
                }
            };
            let persist_succeeded = handler
                .update_graph_with_scan(&mut graph, Some(scan), enrichment)
                .await?;
            if persist_succeeded {
                scanner.commit_state()?;
            }
            let elapsed = t0.elapsed();
            let lsp_edge_count = lsp_call_edge_count(&graph);
            let related_job_ids =
                hydrate_lsp_status_from_ledger(&handler, &repo_root, lsp_edge_count, false);
            let (lsp_state, lsp_detail) = server::operation_report::lsp_capability_from_status(
                enrichment,
                handler.lsp_status.current_state(),
                handler.lsp_status.diagnostic().as_deref(),
                lsp_edge_count,
                !related_job_ids.is_empty(),
            );
            print_scan_summary(ScanSummaryInput {
                repo_root: &repo_root,
                graph: &graph,
                embeddings_loaded: handler.embed_index.load().is_some(),
                elapsed,
                enrichment,
                timings: args.timings,
                operation: server::operation_report::OperationKind::IncrementalRefresh,
                extra_phases: vec![server::operation_report::PhaseReport::ran(
                    server::operation_report::PhaseKind::DiscoverFiles,
                    scan_duration,
                )],
                lsp_state,
                lsp_detail,
                related_job_ids,
                business_context: &handler.business_context,
            });
            return Ok(());
        }
        Some(Commands::LspReadiness(args)) => {
            init_tracing("warn", log_path.as_deref());
            if let Some(cohort_manifest) = args.cohort_manifest.as_deref() {
                let aggregate =
                    repo_native_alignment::lsp_completeness::load_frozen_cohort_aggregate(
                        cohort_manifest,
                    )?;
                let output = args.aggregate_output.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("--aggregate-output is required with --cohort-manifest")
                })?;
                repo_native_alignment::lsp_completeness::persist_aggregate_report(
                    output, &aggregate,
                )?;
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&aggregate)?);
                } else {
                    println!(
                        "LSP aggregate readiness: {} ({}/{} ready, {} unique instances, {} files, digest={})",
                        if aggregate.is_ready() {
                            "READY"
                        } else {
                            "BLOCKED"
                        },
                        aggregate.counts.ready_checkouts,
                        aggregate.counts.checkouts,
                        aggregate.counts.unique_instances,
                        aggregate.counts.files,
                        aggregate.digest,
                    );
                }
                std::process::exit(if aggregate.is_ready() { 0 } else { 2 });
            }
            let repo_root = args.repo.canonicalize()?;
            let graph = server::load_graph_from_lance(&repo_root).await?;
            let check = repo_native_alignment::lsp_completeness::load_readiness_check_with_graph(
                &repo_root,
                business_context_mode,
                &graph.nodes,
                &graph.edges,
            )?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&check)?);
            } else {
                println!("{}", check.human_summary());
                for violation in check
                    .report
                    .violations
                    .iter()
                    .chain(check.compatibility_violations.iter())
                {
                    println!(
                        "- {:?}{}: {}",
                        violation.code,
                        violation
                            .path
                            .as_deref()
                            .map(|path| format!(" [{path}]"))
                            .unwrap_or_default(),
                        violation.detail,
                    );
                }
            }
            std::process::exit(if check.ready { 0 } else { 2 });
        }
        Some(Commands::Enrich(args)) => {
            init_tracing("info", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            let workspace_config = WorkspaceConfig::load()
                .with_primary_root(repo_root.clone())
                .with_worktrees(&repo_root)
                .with_claude_memory(&repo_root)
                .with_agent_memories(&repo_root)
                .with_declared_roots(&repo_root);
            let capability = match args.capability {
                EnrichCapabilityArg::Embeddings => EnrichmentCapability::Embeddings,
                EnrichCapabilityArg::CallReferences => EnrichmentCapability::CallReferences,
            };
            let scope = match args.scope {
                EnrichScopeArg::Repo => EnrichmentScope::Repo,
                EnrichScopeArg::Changed => EnrichmentScope::ChangedFiles,
                EnrichScopeArg::Root => {
                    let root = args.root.clone().ok_or_else(|| {
                        anyhow::anyhow!("--root <slug> is required when --scope root is selected")
                    })?;
                    let known_roots: Vec<String> = workspace_config
                        .resolved_roots()
                        .into_iter()
                        .map(|root| root.slug)
                        .collect();
                    if !known_roots.iter().any(|known| known == &root) {
                        anyhow::bail!(
                            "unknown root slug `{}`; known roots: {}",
                            root,
                            known_roots.join(", ")
                        );
                    }
                    EnrichmentScope::Root(root)
                }
                EnrichScopeArg::Targets => {
                    if args.target_symbols.is_empty() {
                        anyhow::bail!(
                            "--target-symbol is required when --scope targets is selected"
                        );
                    }
                    let mut symbols = args.target_symbols.clone();
                    symbols.sort();
                    symbols.dedup();
                    EnrichmentScope::TargetSymbols(symbols)
                }
                EnrichScopeArg::Task => {
                    if args.target_symbols.is_empty() && args.task_files.is_empty() {
                        anyhow::bail!(
                            "--task-file and/or --target-symbol is required when --scope task is selected"
                        );
                    }
                    let mut files = args.task_files.clone();
                    files.sort();
                    files.dedup();
                    let mut symbols = args.target_symbols.clone();
                    symbols.sort();
                    symbols.dedup();
                    EnrichmentScope::TaskRelevant { files, symbols }
                }
            };
            let lsp_only_roots = workspace_config.lsp_only_roots();
            let handler = RnaHandler {
                repo_root: repo_root.clone(),
                business_context: BusinessContextAdmission::new(business_context_mode),
                lsp_only_roots: Arc::new(lsp_only_roots),
                ..Default::default()
            };
            handler.prepare_business_context_cache()?;
            let graph = match try_load_cached_graph(&repo_root).await? {
                Some(graph) => graph,
                None => anyhow::bail!(
                    "no cached graph found for {}; run `repo-native-alignment scan --extract-only --repo {}` first",
                    repo_root.display(),
                    repo_root.display()
                ),
            };
            handler.graph.store(Arc::new(Some(Arc::new(graph))));
            if capability == EnrichmentCapability::Embeddings {
                if let Some(embed_idx) = load_existing_embedding_index(&repo_root, false, |msg| {
                    tracing::warn!(
                        "{}; explicit embedding enrichment may need a repo-scope run",
                        msg
                    );
                })
                .await
                {
                    handler.embed_index.store(Arc::new(Some(embed_idx)));
                } else if !matches!(scope, EnrichmentScope::Repo) {
                    anyhow::bail!(
                        "no embedding index found for {}; run `repo-native-alignment enrich --capability embeddings --scope repo --repo {}` before scoped embedding enrichment",
                        repo_root.display(),
                        repo_root.display()
                    );
                }
            }
            let enrich_start = std::time::Instant::now();
            let broad_reference_budget = (capability == EnrichmentCapability::CallReferences
                && matches!(
                    scope,
                    EnrichmentScope::ChangedFiles
                        | EnrichmentScope::TargetSymbols(_)
                        | EnrichmentScope::TaskRelevant { .. }
                ))
            .then_some(BroadReferenceBudget {
                max_requests: args.max_requests,
                max_duration_ms: args.max_duration_ms,
            });
            let related_job_ids = handler
                .run_explicit_enrichment_with_broad_reference_budget(
                    capability,
                    scope.clone(),
                    if args.no_background_continuation {
                        EnrichmentContinuation::Disabled
                    } else {
                        EnrichmentContinuation::RunToCompletion
                    },
                    broad_reference_budget,
                    |msg| {
                        eprintln!("{}", msg);
                    },
                )
                .await?;
            let elapsed = enrich_start.elapsed();
            let (symbol_count, edge_count, lsp_edge_count) = {
                let snap = handler.graph.load_full();
                match snap.as_ref().as_ref() {
                    Some(gs) => (
                        gs.nodes.len(),
                        gs.edges.len(),
                        gs.edges
                            .iter()
                            .filter(|edge| {
                                edge.source == repo_native_alignment::graph::ExtractionSource::Lsp
                                    && matches!(
                                        edge.kind,
                                        repo_native_alignment::graph::EdgeKind::Calls
                                    )
                            })
                            .count(),
                    ),
                    None => (0, 0, 0),
                }
            };
            let recent_jobs = handler.enrichment_jobs.recent_jobs(&repo_root, 10);
            let embedding_count = if capability == EnrichmentCapability::Embeddings {
                recent_jobs
                    .iter()
                    .find(|job| related_job_ids.iter().any(|id| id == &job.job_id))
                    .map(|job| job.counters.current)
            } else {
                None
            };
            let mut report = server::operation_report::OperationReport::new(
                server::operation_report::OperationKind::Enrich,
                server::operation_report::OperationTrigger::Cli,
                &repo_root,
            )
            .with_scope(server::operation_report::scope_key(&scope))
            .complete(elapsed);
            report.outputs = server::operation_report::OutputReport {
                symbol_count: Some(symbol_count),
                edge_count: Some(edge_count),
                file_count: None,
                embedding_count,
                lsp_edge_count: Some(lsp_edge_count),
            };
            report.related_job_ids = related_job_ids;
            report.record_business_context(&handler.business_context);
            report.add_phase(server::operation_report::PhaseReport::ran(
                match capability {
                    EnrichmentCapability::Embeddings => {
                        server::operation_report::PhaseKind::Embeddings
                    }
                    EnrichmentCapability::CallReferences => {
                        server::operation_report::PhaseKind::Lsp
                    }
                    EnrichmentCapability::ExtractedGraph => {
                        server::operation_report::PhaseKind::Extract
                    }
                },
                elapsed,
            ));
            report.add_capability(server::operation_report::CapabilityReport::new(
                capability,
                server::operation_report::CapabilityState::Completed,
                true,
                Some(server::operation_report::scope_key(&scope)),
                Some("explicit enrichment completed".to_string()),
            ));
            if let Err(err) = server::OperationReportStore::record(&repo_root, report.clone()) {
                tracing::warn!("failed to persist operation report: {err:#}");
            }
            eprintln!();
            eprintln!("{}", report.render_cli(true));
            return Ok(());
        }
        Some(Commands::Search(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            let gs = load_cache_backed_query_graph(&repo_root, business_context_mode).await?;
            // Load existing embedding index -- do NOT rebuild.
            let embed_idx = match repo_native_alignment::embed::EmbeddingIndex::new(&repo_root)
                .await
            {
                Ok(idx) => match idx.has_table().await {
                    Ok(true) => Some(idx),
                    Ok(false) => {
                        eprintln!(
                            "No embedding index found. Run `repo-native-alignment scan --path .` first."
                        );
                        None
                    }
                    Err(e) => {
                        eprintln!(
                            "Embedding index check failed: {}. Semantic search will be disabled.",
                            e
                        );
                        None
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        "EmbeddingIndex init failed; semantic search may degrade: {}",
                        e
                    );
                    None
                }
            };
            // Validate: --include-body requires --node or --nodes
            if args.include_body
                && args.node.is_none()
                && args
                    .nodes
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .is_none()
            {
                anyhow::bail!("--include-body requires --node or --nodes");
            }

            let embed_ref = embed_idx.as_ref();
            let params = SearchParams {
                query: if args.query.is_empty() {
                    None
                } else {
                    Some(args.query.clone())
                },
                node: args.node.clone(),
                mode: args.mode.clone(),
                hops: args.hops,
                depth: args.depth,
                direction: args.direction.clone(),
                edge_types: args
                    .edge_types
                    .as_ref()
                    .map(|s| s.split(',').map(|t| t.trim().to_string()).collect()),
                kind: args.kind.clone(),
                language: args.language.clone(),
                file: args.file.clone(),
                line: args.line,
                end_line: args.end_line,
                root: args.root.clone(),
                limit: Some(args.limit),
                sort_by: args.sort_by.clone(),
                min_complexity: args.min_complexity,
                synthetic: args.synthetic,
                compact: args.compact,
                nodes: args
                    .nodes
                    .as_ref()
                    .map(|s| s.split(',').map(|t| t.trim().to_string()).collect()),
                search_mode: args.search_mode.clone(),
                rerank: args.rerank,
                include_artifacts: args.include_artifacts,
                include_markdown: args.include_markdown,
                artifact_types: args
                    .artifact_types
                    .as_ref()
                    .map(|s| s.split(',').map(|t| t.trim().to_string()).collect()),
                subsystem: args.subsystem.clone(),
                target_subsystem: args.target_subsystem.clone(),
                include_body: args.include_body,
                minify_body: args.minify_body,
                verbose: args.verbose,
                projection: args.projection.clone(),
                body_policy: args.body_policy.clone(),
                max_output_bytes: args.max_output_bytes,
                max_output_tokens: args.max_output_tokens,
                max_body_bytes: args.max_body_bytes,
                max_total_body_bytes: args.max_total_body_bytes,
                context_mode: args.context_mode.clone(),
                context_roles: args.context_roles.as_ref().map(|values| {
                    values
                        .split(',')
                        .map(|value| value.trim().to_string())
                        .collect()
                }),
                context_facets: args.context_facets.as_ref().map(|values| {
                    values
                        .split(',')
                        .map(|value| value.trim().to_string())
                        .collect()
                }),
                proposal: args.proposal.clone(),
            };
            let root_filter = resolve_root_filter(args.root.as_deref(), &repo_root);
            // Include lsp_only subdirectory root slugs in non_code_slugs so they're not
            // filtered out by the default root filter (which scopes to the primary root slug).
            let non_code_slugs: std::collections::HashSet<String> =
                repo_native_alignment::roots::WorkspaceConfig::load()
                    .with_primary_root(repo_root.clone())
                    .with_declared_roots(&repo_root)
                    .lsp_only_roots()
                    .into_iter()
                    .map(|(slug, _)| slug)
                    .collect();
            let business_context = BusinessContextAdmission::new(business_context_mode);
            let ctx = SearchContext {
                graph_state: &gs,
                embed_index: embed_ref,
                repo_root: &repo_root,
                lsp_status: None,
                embed_status: None,
                root_filter,
                non_code_slugs,
                enrichment_jobs: EnrichmentJobLedger::default().all_jobs(&repo_root),
                business_context: &business_context,
            };
            println!("{}", service::search(&params, &ctx).await);
            return Ok(());
        }
        Some(Commands::Graph(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            let gs = load_cache_backed_query_graph(&repo_root, business_context_mode).await?;
            let gp = GraphParams {
                node: args.node.clone(),
                mode: args.mode.clone(),
                direction: args.direction.clone(),
                edge_types: args
                    .edge_types
                    .as_ref()
                    .map(|s| s.split(',').map(|t| t.trim().to_string()).collect()),
                max_hops: args.max_hops,
            };
            match service::graph_query(&gp, &gs) {
                Ok(output) => println!("{}", output),
                Err(msg) => {
                    eprintln!("Error: {}", msg);
                    std::process::exit(1);
                }
            }
            eprintln!(
                "{}",
                server::format_freshness(gs.nodes.len(), gs.last_scan_completed_at, None)
            );
            return Ok(());
        }
        Some(Commands::Stats(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            let gs = load_cache_backed_query_graph(&repo_root, business_context_mode).await?;
            let st = service::stats(&repo_root, &gs).await;
            println!(
                "  Symbols: {} | Edges: {} | Embeddings: {} | Languages: {} | Last scan: {} | .oh/: {} artifacts ({} outcomes, {} signals, {} guardrails, {} metis)",
                st.node_count,
                st.edge_count,
                if st.embeddings_available { "yes" } else { "no" },
                if st.languages.is_empty() {
                    "none".to_string()
                } else {
                    st.languages.join(", ")
                },
                st.last_scan_age,
                st.artifact_count,
                st.outcome_count,
                st.signal_count,
                st.guardrail_count,
                st.metis_count
            );
            return Ok(());
        }
        Some(Commands::OutcomeProgress(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            let gs = load_cache_backed_query_graph(&repo_root, business_context_mode).await?;
            let root_filter = resolve_root_filter(args.root.as_deref(), &repo_root);
            let params = OutcomeProgressParams {
                outcome_id: args.outcome_id.clone(),
                include_impact: args.include_impact,
                root_filter,
                non_code_slugs: std::collections::HashSet::new(),
            };
            let business_context = BusinessContextAdmission::new(business_context_mode);
            let ctx = OutcomeProgressContext {
                graph_state: &gs,
                repo_root: &repo_root,
                business_context: &business_context,
            };
            println!("{}", service::outcome_progress(&params, &ctx));
            return Ok(());
        }
        Some(Commands::ListRoots(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            let gs = load_cache_backed_query_graph(&repo_root, business_context_mode).await?;
            let index_map = gs.node_index_map();
            // Start with graph-derived slugs (roots that have extracted nodes).
            let mut active_slugs: std::collections::HashSet<String> =
                repo_native_alignment::server::state::GraphState::root_slugs_from_index_map(
                    index_map,
                )
                .into_iter()
                .collect();
            // Union with all configured roots so declared roots with zero nodes still appear.
            for r in WorkspaceConfig::load()
                .with_primary_root(repo_root.clone())
                .with_worktrees(&repo_root)
                .with_claude_memory(&repo_root)
                .with_agent_memories(&repo_root)
                .with_declared_roots(&repo_root)
                .resolved_roots()
            {
                active_slugs.insert(r.slug);
            }
            println!(
                "{}",
                service::list_roots_from_slugs(&repo_root, &active_slugs, Some(&gs), None, None)
            );
            return Ok(());
        }
        Some(Commands::RepoMap(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            let gs = load_cache_backed_query_graph(&repo_root, business_context_mode).await?;
            let root_filter = resolve_root_filter(args.root.as_deref(), &repo_root);
            let params = RepoMapParams {
                top_n: args.top_n,
                root_filter,
                non_code_slugs: std::collections::HashSet::new(),
            };
            let business_context = BusinessContextAdmission::new(business_context_mode);
            let ctx = RepoMapContext {
                graph_state: &gs,
                repo_root: &repo_root,
                lsp_status: None,
                embed_status: None,
                business_context: &business_context,
            };
            println!("{}", service::repo_map(&params, &ctx));
            return Ok(());
        }
        Some(Commands::Adr(args)) => {
            init_tracing("warn", log_path.as_deref());
            match args.command {
                AdrCommand::Compile(args) => {
                    let repo_root = args.repo.canonicalize()?;
                    let adr_dir = if args.dir.is_absolute() {
                        args.dir.clone()
                    } else {
                        repo_root.join(&args.dir)
                    };
                    let report = adr::compile(&repo_root, &adr_dir, args.check)?;
                    if args.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        println!("{}", report.human_summary());
                    }
                    std::process::exit(if report.ok() { 0 } else { 2 });
                }
                AdrCommand::Validate(args) => {
                    let repo_root = args.repo.canonicalize()?;
                    let selection = ValidateSelection {
                        id: args.id.clone(),
                        source_path: args.path.clone().map(|path| {
                            if path.is_absolute() {
                                path
                            } else {
                                repo_root.join(path)
                            }
                        }),
                    };
                    let report = adr::validate(&repo_root, &selection, &args.cargo_args)?;
                    if args.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        println!("{}", report.human_summary());
                    }
                    std::process::exit(if report.ok() { 0 } else { 3 });
                }
                AdrCommand::Audit(args) => {
                    let repo_root = args.repo.canonicalize()?;
                    let report = adr::run_audit(&repo_root, &args.name)?;
                    if args.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        println!("{}", report.human_summary());
                    }
                    std::process::exit(if report.ok { 0 } else { 3 });
                }
            }
        }
        Some(Commands::Open(args)) => {
            init_tracing("warn", log_path.as_deref());
            repo_native_alignment::open_viewer::run(args.repo, business_context_mode).await?;
            return Ok(());
        }
        None => {}
    }
    let repo_root = cli.repo.canonicalize()?;
    let lsp_only_roots = WorkspaceConfig::load()
        .with_primary_root(repo_root.clone())
        .with_declared_roots(&repo_root)
        .lsp_only_roots();
    let handler = RnaHandler {
        repo_root: repo_root.clone(),
        cache_only: cli.cache_only,
        business_context: BusinessContextAdmission::new(business_context_mode),
        lsp_only_roots: Arc::new(lsp_only_roots),
        ..Default::default()
    };
    match cli.transport.as_str() {
        "stdio" => init_tracing("warn", log_path.as_deref()),
        "http" => init_tracing("info", log_path.as_deref()),
        other => anyhow::bail!("Unknown transport: {}. Use 'stdio' or 'http'.", other),
    }
    handler.prepare_business_context_cache()?;
    let graph_load_started = std::time::Instant::now();
    match try_load_cached_graph(&repo_root).await {
        Ok(Some(state)) => {
            tracing::info!(
                target: "rna_query_timing",
                phase = "graph_load",
                elapsed_ms = graph_load_started.elapsed().as_secs_f64() * 1000.0
            );
            handler.install_cached_graph(state);
            let embedding_open_started = std::time::Instant::now();
            if let Some(embed_idx) =
                load_existing_embedding_index(&repo_root, cli.cache_only, |msg| {
                    tracing::warn!("{}; MCP semantic search will be unavailable", msg);
                })
                .await
            {
                handler.embed_index.store(Arc::new(Some(embed_idx)));
            }
            tracing::info!(
                target: "rna_query_timing",
                phase = "embedding_open",
                elapsed_ms = embedding_open_started.elapsed().as_secs_f64() * 1000.0
            );
        }
        Ok(None) if cli.cache_only => {
            anyhow::bail!("cache-only runtime requires an existing persisted graph")
        }
        Ok(None) => {}
        Err(err) if cli.cache_only => return Err(err),
        Err(err) => tracing::warn!(
            "MCP startup could not preload the cached graph: {err:#}; background prewarm will recover it"
        ),
    }
    match cli.transport.as_str() {
        "stdio" => {
            let transport = rust_mcp_sdk::StdioTransport::new(Default::default())
                .map_err(|e| anyhow::anyhow!("{:?}", e))?;
            let server = rust_mcp_sdk::mcp_server::server_runtime::create_server(
                rust_mcp_sdk::mcp_server::McpServerOptions {
                    server_details: server_details(),
                    transport,
                    handler: handler.to_mcp_server_handler(),
                    task_store: None,
                    client_task_store: None,
                },
            );
            server
                .start()
                .await
                .map_err(|e| anyhow::anyhow!("{:?}", e))?;
        }
        "http" => {
            let server = rust_mcp_sdk::mcp_server::hyper_server::create_server(
                server_details(),
                handler.to_mcp_server_handler(),
                rust_mcp_sdk::mcp_server::HyperServerOptions {
                    host: cli.host,
                    port: cli.port,
                    event_store: Some(Arc::new(
                        rust_mcp_sdk::event_store::InMemoryEventStore::default(),
                    )),
                    ..Default::default()
                },
            );
            server
                .start()
                .await
                .map_err(|e| anyhow::anyhow!("{:?}", e))?;
        }
        _ => unreachable!("transport was validated before MCP cache loading"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use repo_native_alignment::server::{EnrichmentTrigger, JobStart, LspState};

    #[test]
    fn cache_only_server_flag_is_explicit_and_opt_in() {
        let ordinary = Cli::try_parse_from(["repo-native-alignment"]).unwrap();
        assert!(!ordinary.cache_only);

        let cache_only = Cli::try_parse_from([
            "repo-native-alignment",
            "--cache-only",
            "--transport",
            "http",
        ])
        .unwrap();
        assert!(cache_only.cache_only);
        assert_eq!(cache_only.transport, "http");
    }

    #[test]
    fn search_cli_preserves_defaults_and_accepts_explicit_content_exclusion() {
        for (arguments, expected_artifacts, expected_markdown) in [
            (vec!["repo-native-alignment", "search", "query"], true, true),
            (
                vec![
                    "repo-native-alignment",
                    "search",
                    "query",
                    "--include-artifacts",
                ],
                true,
                true,
            ),
            (
                vec![
                    "repo-native-alignment",
                    "search",
                    "query",
                    "--include-artifacts=false",
                ],
                false,
                true,
            ),
            (
                vec![
                    "repo-native-alignment",
                    "search",
                    "query",
                    "--include-markdown",
                ],
                true,
                true,
            ),
            (
                vec![
                    "repo-native-alignment",
                    "search",
                    "query",
                    "--include-markdown=false",
                ],
                true,
                false,
            ),
        ] {
            let cli = Cli::try_parse_from(arguments).expect("search CLI should parse");
            let Some(Commands::Search(args)) = cli.command else {
                panic!("expected search command");
            };
            assert_eq!(args.include_artifacts, expected_artifacts);
            assert_eq!(args.include_markdown, expected_markdown);
        }
    }

    #[test]
    fn lsp_readiness_cli_exposes_checkout_and_aggregate_modes() {
        let checkout = Cli::try_parse_from([
            "repo-native-alignment",
            "--business-context",
            "disabled",
            "lsp-readiness",
            "--repo",
            "/tmp/checkout",
            "--json",
        ])
        .expect("checkout readiness CLI should parse");
        let Some(Commands::LspReadiness(args)) = checkout.command else {
            panic!("expected lsp-readiness command");
        };
        assert_eq!(checkout.business_context, BusinessContextMode::Disabled);
        assert_eq!(args.repo, PathBuf::from("/tmp/checkout"));
        assert!(args.json);
        assert!(args.cohort_manifest.is_none());

        let aggregate = Cli::try_parse_from([
            "repo-native-alignment",
            "lsp-readiness",
            "--cohort-manifest",
            "/tmp/frozen-cohort.json",
            "--aggregate-output",
            "/tmp/aggregate.json",
        ])
        .expect("aggregate readiness CLI should parse");
        let Some(Commands::LspReadiness(args)) = aggregate.command else {
            panic!("expected lsp-readiness command");
        };
        assert_eq!(
            args.cohort_manifest,
            Some(PathBuf::from("/tmp/frozen-cohort.json"))
        );
        assert_eq!(
            args.aggregate_output,
            Some(PathBuf::from("/tmp/aggregate.json"))
        );
    }

    #[test]
    fn enrich_cli_exposes_target_scope_and_visible_budget() {
        let cli = Cli::try_parse_from([
            "repo-native-alignment",
            "enrich",
            "--capability",
            "call-references",
            "--scope",
            "targets",
            "--target-symbol",
            "root:src/lib.rs:Thing:struct",
            "--max-requests",
            "7",
            "--max-duration-ms",
            "900",
        ])
        .expect("target-scope CLI should parse");
        let Some(Commands::Enrich(args)) = cli.command else {
            panic!("expected enrich command");
        };
        assert!(matches!(args.scope, EnrichScopeArg::Targets));
        assert_eq!(
            args.target_symbols,
            vec!["root:src/lib.rs:Thing:struct".to_string()]
        );
        assert_eq!(args.max_requests, 7);
        assert_eq!(args.max_duration_ms, 900);
    }

    #[test]
    fn enrich_cli_task_scope_accepts_files_and_symbols() {
        let cli = Cli::try_parse_from([
            "repo-native-alignment",
            "enrich",
            "--capability",
            "call-references",
            "--scope",
            "task",
            "--task-file",
            "src/lib.rs",
            "--target-symbol",
            "Thing",
        ])
        .expect("task-scope CLI should parse");
        let Some(Commands::Enrich(args)) = cli.command else {
            panic!("expected enrich command");
        };
        assert!(matches!(args.scope, EnrichScopeArg::Task));
        assert_eq!(args.task_files, vec!["src/lib.rs".to_string()]);
        assert_eq!(args.target_symbols, vec!["Thing".to_string()]);
        assert_eq!(args.max_requests, 512);
        assert_eq!(args.max_duration_ms, 120_000);
    }

    #[test]
    fn completed_repo_job_hydrates_as_default_profile_without_manual_evidence_write() {
        let tmp = tempfile::tempdir().expect("temp repo");
        let handler = RnaHandler {
            repo_root: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let job_id = match handler
            .enrichment_jobs
            .begin_job(
                tmp.path(),
                EnrichmentCapability::CallReferences,
                EnrichmentScope::Repo,
                EnrichmentTrigger::ForegroundScan,
                None,
            )
            .expect("begin repo warm-up job")
        {
            JobStart::Started(job) => job.job_id,
            JobStart::Joined { existing_job_id } => existing_job_id,
        };
        handler
            .enrichment_jobs
            .mark_completed(tmp.path(), &job_id, 10, 7);
        // Simulate a sentinel/cache path that optimistically restored COMPLETE.
        handler.lsp_status.set_complete(7);

        hydrate_lsp_status_from_ledger(&handler, tmp.path(), 7, false);

        let readiness = handler.lsp_status.call_reference_readiness();
        assert_eq!(
            readiness.state,
            server::state::CapabilityReadinessState::Partial
        );
        assert!(readiness.detail.contains("default query profile"));
        assert!(readiness.detail.contains("broad references were omitted"));
        assert_eq!(
            handler.lsp_status.dead_code_readiness().state,
            server::state::CapabilityReadinessState::Partial
        );
        let job = handler
            .enrichment_jobs
            .recent_jobs(tmp.path(), 1)
            .pop()
            .expect("persisted repo job");
        assert_eq!(
            job.lsp_evidence.map(|evidence| evidence.readiness),
            Some(server::LspEvidenceReadiness::DefaultProfile)
        );
    }

    #[test]
    fn ledger_hydration_replaces_optimistic_complete_with_durable_partial_evidence() {
        let tmp = tempfile::tempdir().expect("temp repo");
        let handler = RnaHandler {
            repo_root: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let historical_job = match handler
            .enrichment_jobs
            .begin_job(
                tmp.path(),
                EnrichmentCapability::CallReferences,
                EnrichmentScope::Repo,
                EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin historical job")
        {
            JobStart::Started(job) => job.job_id,
            JobStart::Joined { existing_job_id } => existing_job_id,
        };
        handler.enrichment_jobs.mark_degraded(
            tmp.path(),
            &historical_job,
            2,
            1,
            "historical abort",
        );
        handler.lsp_status.set_complete(3);

        let related = hydrate_lsp_status_from_ledger(&handler, tmp.path(), 3, false);

        assert_eq!(handler.lsp_status.current_state(), LspState::Degraded);
        let readiness = handler.lsp_status.call_reference_readiness();
        assert!(readiness.detail.contains("historical abort"));
        assert!(readiness.detail.contains("for repo-wide"));
        assert!(
            !handler
                .lsp_status
                .review_readiness()
                .detail
                .contains("explicit scoped/degraded context")
        );
        assert_eq!(related, vec![historical_job]);
    }

    #[test]
    fn cache_only_hydration_overrides_fresh_probe_unavailable_with_durable_degraded_job() {
        let tmp = tempfile::tempdir().expect("temp repo");
        let handler = RnaHandler {
            repo_root: tmp.path().to_path_buf(),
            ..Default::default()
        };
        let job_id = match handler
            .enrichment_jobs
            .begin_job(
                tmp.path(),
                EnrichmentCapability::CallReferences,
                EnrichmentScope::Repo,
                EnrichmentTrigger::Explicit,
                None,
            )
            .expect("begin durable degraded job")
        {
            JobStart::Started(job) => job.job_id,
            JobStart::Joined { existing_job_id } => existing_job_id,
        };
        handler.enrichment_jobs.mark_degraded(
            tmp.path(),
            &job_id,
            2,
            1,
            "durable no-progress abort",
        );
        handler.lsp_status.set_unavailable();

        hydrate_lsp_status_from_ledger(&handler, tmp.path(), 1, true);

        assert_eq!(handler.lsp_status.current_state(), LspState::Degraded);
        assert_eq!(
            handler.lsp_status.diagnostic().as_deref(),
            Some("durable no-progress abort")
        );
    }
}
