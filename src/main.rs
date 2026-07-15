use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::ToMcpServerHandler;
use rust_mcp_sdk::schema::{Implementation, InitializeResult, ServerCapabilities};

use repo_native_alignment::adr::{self, ValidateSelection};
use repo_native_alignment::roots::WorkspaceConfig;
use repo_native_alignment::server::{
    self, EnrichmentCapability, EnrichmentContinuation, EnrichmentJobLedger, EnrichmentScope,
    RnaHandler, ScanEnrichmentOptions,
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
}

#[derive(Subcommand, Debug)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    Setup(SetupArgs),
    Test(TestArgs),
    Scan(ScanArgs),
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
    #[arg(long, default_value_t = true)]
    include_artifacts: bool,
    #[arg(long, default_value_t = true)]
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

async fn load_existing_embedding_index(
    repo_root: &std::path::Path,
    warn: impl FnOnce(String),
) -> Option<repo_native_alignment::embed::EmbeddingIndex> {
    match repo_native_alignment::embed::EmbeddingIndex::new(repo_root).await {
        Ok(idx) => match idx.has_table().await {
            Ok(true) => Some(idx),
            Ok(false) => None,
            Err(e) => {
                warn(format!("Embedding index check failed: {}", e));
                None
            }
        },
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
    match cli.command {
        Some(Commands::Setup(args)) => return setup::run(&args),
        Some(Commands::Test(args)) => {
            init_tracing("info", log_path.as_deref());
            let passed = smoke_test::run(&args).await?;
            std::process::exit(if passed { 0 } else { 1 });
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
                    lsp_only_roots: Arc::new(lsp_only_roots_scan),
                    ..Default::default()
                };
                let result = handler
                    .run_pipeline_foreground(
                        |msg| {
                            eprintln!("{}", msg);
                        },
                        enrichment,
                    )
                    .await?;
                if let Err(err) =
                    server::OperationReportStore::record(&repo_root, result.report.clone())
                {
                    tracing::warn!("failed to persist operation report: {err:#}");
                }
                if args.timings {
                    eprintln!();
                    eprintln!("{}", result.report.render_cli(true));
                }
                return Ok(());
            }
            eprintln!("Scanning: {}", repo_root.display());
            let t0 = std::time::Instant::now();
            let handler = RnaHandler {
                repo_root: repo_root.clone(),
                lsp_only_roots: Arc::new(lsp_only_roots_scan),
                ..Default::default()
            };
            if let Some(embed_idx) = load_existing_embedding_index(&repo_root, |msg| {
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
                let related_job_ids = Vec::new();
                let (lsp_state, lsp_detail) = server::operation_report::lsp_capability_from_status(
                    enrichment,
                    handler.lsp_status.current_state(),
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
                    let related_job_ids = Vec::new();
                    let (lsp_state, lsp_detail) =
                        server::operation_report::lsp_capability_from_status(
                            enrichment,
                            handler.lsp_status.current_state(),
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
            let related_job_ids = Vec::new();
            let (lsp_state, lsp_detail) = server::operation_report::lsp_capability_from_status(
                enrichment,
                handler.lsp_status.current_state(),
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
            });
            return Ok(());
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
            };
            let lsp_only_roots = workspace_config.lsp_only_roots();
            let handler = RnaHandler {
                repo_root: repo_root.clone(),
                lsp_only_roots: Arc::new(lsp_only_roots),
                ..Default::default()
            };
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
                if let Some(embed_idx) = load_existing_embedding_index(&repo_root, |msg| {
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
            let related_job_ids = handler
                .run_explicit_enrichment(
                    capability,
                    scope.clone(),
                    if args.no_background_continuation {
                        EnrichmentContinuation::Disabled
                    } else {
                        EnrichmentContinuation::RunToCompletion
                    },
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
            let gs = load_cached_graph(&repo_root).await;
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
            let ctx = SearchContext {
                graph_state: &gs,
                embed_index: embed_ref,
                repo_root: &repo_root,
                lsp_status: None,
                embed_status: None,
                root_filter,
                non_code_slugs,
                enrichment_jobs: EnrichmentJobLedger::default().all_jobs(&repo_root),
            };
            println!("{}", service::search(&params, &ctx).await);
            return Ok(());
        }
        Some(Commands::Graph(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            let gs = load_cached_graph(&repo_root).await;
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
            let lance_path = repo_root.join(".oh").join(".cache").join("lance");
            if !lance_path.exists() {
                eprintln!("No index found. Run `repo-native-alignment scan --path .` first.");
                std::process::exit(1);
            }
            let gs = server::load_graph_from_lance(&repo_root).await?;
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
            let gs = load_cached_graph(&repo_root).await;
            let root_filter = resolve_root_filter(args.root.as_deref(), &repo_root);
            let params = OutcomeProgressParams {
                outcome_id: args.outcome_id.clone(),
                include_impact: args.include_impact,
                root_filter,
                non_code_slugs: std::collections::HashSet::new(),
            };
            let ctx = OutcomeProgressContext {
                graph_state: &gs,
                repo_root: &repo_root,
            };
            println!("{}", service::outcome_progress(&params, &ctx));
            return Ok(());
        }
        Some(Commands::ListRoots(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            let gs = load_cached_graph(&repo_root).await;
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
            let gs = load_cached_graph(&repo_root).await;
            let root_filter = resolve_root_filter(args.root.as_deref(), &repo_root);
            let params = RepoMapParams {
                top_n: args.top_n,
                root_filter,
                non_code_slugs: std::collections::HashSet::new(),
            };
            let ctx = RepoMapContext {
                graph_state: &gs,
                repo_root: &repo_root,
                lsp_status: None,
                embed_status: None,
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
            repo_native_alignment::open_viewer::run(args.repo).await?;
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
        lsp_only_roots: Arc::new(lsp_only_roots),
        ..Default::default()
    };
    match cli.transport.as_str() {
        "stdio" => {
            init_tracing("warn", log_path.as_deref());
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
            init_tracing("info", log_path.as_deref());
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
        other => {
            anyhow::bail!("Unknown transport: {}. Use 'stdio' or 'http'.", other);
        }
    }
    Ok(())
}
