use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::{Implementation, InitializeResult, ServerCapabilities};
use rust_mcp_sdk::ToMcpServerHandler;

use repo_native_alignment::server::{self, RnaHandler};
use repo_native_alignment::service::{self, SearchContext, SearchParams, GraphParams};
use repo_native_alignment::setup::{self, SetupArgs};
use repo_native_alignment::smoke_test::{self, TestArgs};

#[derive(Parser, Debug)]
#[command(
    name = "repo-native-alignment",
    version,
    about = "Repo-Native Alignment MCP Server",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    // ── Server args (used when no subcommand is given) ───────────────────────

    /// Repository root path (server mode only; default: current directory)
    #[arg(long, default_value = ".")]
    repo: PathBuf,

    /// Transport mode: "stdio" (default) or "http"
    #[arg(long, default_value = "stdio")]
    transport: String,

    /// Host to bind to (http mode only)
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to bind to (http mode only)
    #[arg(long, default_value_t = 8382)]
    port: u16,

    /// Write logs to a file (in addition to stderr). Also settable via RNA_LOG_FILE env var.
    #[arg(long)]
    log_path: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Bootstrap RNA + OH MCP setup for a project
    Setup(SetupArgs),
    /// Run the full pipeline smoke test (scan -> extract -> embed -> index -> query).
    ///
    /// Exits 0 on pass, 1 on any failure. Runnable in CI with no extra dependencies.
    Test(TestArgs),
    /// Scan, extract, embed, and persist the full graph for a repo.
    ///
    /// Runs the same pipeline as MCP server startup but standalone, with timing output.
    /// Use --full to also run embedding and LSP enrichment synchronously.
    Scan(ScanArgs),
    /// Search code symbols by name or signature.
    ///
    /// Scans the repo on first use, then filters symbols by query, kind, language, and file.
    Search(SearchArgs),
    /// Traverse the code graph: neighbors, impact analysis, or reachability.
    ///
    /// Use `search` first to find a node ID, then `graph` to explore relationships.
    Graph(GraphArgs),
    /// Show repo stats from the persisted index (no re-scan).
    Stats(StatsArgs),
}

#[derive(clap::Args, Debug)]
struct StatsArgs {
    /// Repository root path (default: current directory)
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}

#[derive(clap::Args, Debug)]
struct ScanArgs {
    /// Repository root path to scan (default: current directory or --repo)
    #[arg(long)]
    path: Option<PathBuf>,

    /// Run the COMPLETE pipeline: scan + extract + embed + LSP enrich + graph.
    /// Without this flag, only scan + extract + persist runs (no LSP, no foreground embed).
    #[arg(long)]
    full: bool,

    /// Repository root path (default: current directory)
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}

#[derive(clap::Args, Debug)]
struct SearchArgs {
    /// Search query (matched against symbol name and signature)
    #[arg(default_value = "")]
    query: String,
    #[arg(long, default_value = ".")] repo: PathBuf,
    #[arg(long)] kind: Option<String>,
    #[arg(long)] language: Option<String>,
    #[arg(long)] file: Option<String>,
    #[arg(long, default_value_t = 20)] limit: usize,
    #[arg(long)] node: Option<String>,
    #[arg(long)] mode: Option<String>,
    #[arg(long)] hops: Option<u32>,
    #[arg(long)] direction: Option<String>,
    #[arg(long)] edge_types: Option<String>,
    #[arg(long)] sort_by: Option<String>,
    #[arg(long)] min_complexity: Option<u32>,
    #[arg(long)] synthetic: Option<bool>,
    #[arg(long)] compact: bool,
    #[arg(long)] nodes: Option<String>,
    #[arg(long)] search_mode: Option<String>,
    #[arg(long, default_value_t = true)] include_artifacts: bool,
    #[arg(long, default_value_t = true)] include_markdown: bool,
    #[arg(long)] artifact_types: Option<String>,
    #[arg(long)] root: Option<String>,
}

#[derive(clap::Args, Debug)]
struct GraphArgs {
    /// Stable node ID from search results
    #[arg(long)]
    node: String,
    /// Query mode: neighbors, impact, reachable (default: neighbors)
    #[arg(long, default_value = "neighbors")]
    mode: String,
    /// Direction: outgoing, incoming, both (neighbors mode only, default: outgoing)
    #[arg(long, default_value = "outgoing")]
    direction: String,
    /// Filter by edge types (comma-separated): calls, depends_on, implements, etc.
    #[arg(long)]
    edge_types: Option<String>,
    /// Maximum hops (default: 1 for neighbors, 3 for impact/reachable)
    #[arg(long)]
    max_hops: Option<usize>,
    /// Repository root path (default: current directory)
    #[arg(long, default_value = ".")]
    repo: PathBuf,
}

fn server_details() -> InitializeResult {
    InitializeResult {
        capabilities: ServerCapabilities {
            tools: Some(Default::default()),
            ..Default::default()
        },
        instructions: Some(
            "Repo-Native Alignment: query business outcomes, code, markdown, and git history."
                .to_string(),
        ),
        meta: None,
        protocol_version: "2025-11-25".to_string(),
        server_info: Implementation {
            name: "rna-server".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: Some(
                "MCP server for querying business outcomes, code, and git history".to_string(),
            ),
            icons: vec![],
            title: Some("Repo-Native Alignment".to_string()),
            website_url: None,
        },
    }
}

/// Initialize tracing with optional file logging.
///
/// When `log_path` is Some, logs are written to both stderr and the file.
/// The RNA_LOG_FILE env var is checked as a fallback if `log_path` is None.
fn init_tracing(default_filter: &str, log_path: Option<&std::path::Path>) {
    use tracing_subscriber::prelude::*;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| default_filter.into());

    // Resolve log file: CLI flag takes precedence, then env var
    let effective_log_path = log_path
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::var("RNA_LOG_FILE").ok().map(PathBuf::from));

    if let Some(ref file_path) = effective_log_path {
        // Ensure parent directory exists
        if let Some(parent) = file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file_path)
        {
            Ok(file) => {
                let file_layer = tracing_subscriber::fmt::layer()
                    .with_writer(std::sync::Mutex::new(file))
                    .with_ansi(false);

                let stderr_layer = tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr);

                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(stderr_layer)
                    .with(file_layer)
                    .init();

                tracing::info!("Logging to file: {}", file_path.display());
                return;
            }
            Err(e) => {
                eprintln!("Warning: could not open log file {}: {}", file_path.display(), e);
                // Fall through to stderr-only logging
            }
        }
    }

    // Default: stderr only
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let log_path = cli.log_path.clone();

    // ── subcommands ──────────────────────────────────────────────────────────
    match cli.command {
        Some(Commands::Setup(args)) => return setup::run(&args),
        Some(Commands::Test(args)) => {
            init_tracing("info", log_path.as_deref());
            tracing::info!("Running RNA pipeline smoke test for {}", args.repo.display());
            let passed = smoke_test::run(&args).await?;
            std::process::exit(if passed { 0 } else { 1 });
        }
        Some(Commands::Scan(args)) => {
            init_tracing("info", log_path.as_deref());
            let repo_root = args.path.unwrap_or_else(|| args.repo.clone()).canonicalize()?;

            if args.full {
                // --full: run the complete pipeline synchronously with progress output
                eprintln!("Full pipeline scan: {}", repo_root.display());
                let handler = RnaHandler {
                    repo_root: repo_root.clone(),
                    ..Default::default()
                };
                let _result = handler.run_pipeline_foreground(|msg| {
                    eprintln!("{}", msg);
                }).await?;
                return Ok(());
            }

            // Default scan: extract + persist only (no LSP, no foreground embed)
            eprintln!("Scanning: {}", repo_root.display());
            let t0 = std::time::Instant::now();
            let handler = RnaHandler {
                repo_root: repo_root.clone(),
                ..Default::default()
            };
            let graph = handler.build_full_graph().await?;

            // build_full_graph spawns embedding as a background task (for MCP server).
            // For the CLI scan command, run embedding in the foreground so it completes
            // before the process exits (avoids lance panic from cancelled tokio tasks).
            let embed_count = match repo_native_alignment::embed::EmbeddingIndex::new(&repo_root).await {
                Ok(idx) => {
                    eprintln!("  Embedding {} symbols...", graph.nodes.iter().filter(|n| n.id.root != "external").count());
                    loop {
                        match idx.search("_probe_", None, 1).await {
                            Ok(repo_native_alignment::embed::SearchOutcome::Results(_)) => break 1,
                            Ok(repo_native_alignment::embed::SearchOutcome::NotReady) | Err(_) => {
                                tokio::time::sleep(std::time::Duration::from_secs(2)).await
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("  Embedding init failed: {}", e);
                    0
                }
            };

            let elapsed = t0.elapsed();
            eprintln!();
            eprintln!("── Scan complete ──────────────────────────");
            eprintln!("  Symbols:    {}", graph.nodes.len());
            eprintln!("  Edges:      {}", graph.edges.len());
            eprintln!("  Embeddings: {}", if embed_count > 0 { format!("{} vectors", embed_count) } else { "no".to_string() });
            eprintln!("  Time:       {:.2}s", elapsed.as_secs_f64());
            eprintln!("───────────────────────────────────────────");
            return Ok(());
        }
        Some(Commands::Search(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            eprintln!("Scanning {}...", repo_root.display());
            let handler = RnaHandler { repo_root: repo_root.clone(), ..Default::default() };
            let gs = handler.build_full_graph().await?;
            let params = SearchParams {
                query: if args.query.is_empty() { None } else { Some(args.query.clone()) },
                node: args.node.clone(), mode: args.mode.clone(), hops: args.hops,
                direction: args.direction.clone(),
                edge_types: args.edge_types.as_ref().map(|s| s.split(',').map(|t| t.trim().to_string()).collect()),
                kind: args.kind.clone(), language: args.language.clone(), file: args.file.clone(),
                limit: Some(args.limit), sort_by: args.sort_by.clone(), min_complexity: args.min_complexity,
                synthetic: args.synthetic, compact: args.compact,
                nodes: args.nodes.as_ref().map(|s| s.split(',').map(|t| t.trim().to_string()).collect()),
                search_mode: args.search_mode.clone(),
                include_artifacts: args.include_artifacts, include_markdown: args.include_markdown,
                artifact_types: args.artifact_types.as_ref().map(|s| s.split(',').map(|t| t.trim().to_string()).collect()),
            };
            let root_slug = repo_native_alignment::roots::RootConfig::code_project(repo_root.clone()).slug();
            let root_filter = args.root.as_deref()
                .map(|v| if v.eq_ignore_ascii_case("all") { None } else { Some(v.to_string()) })
                .unwrap_or_else(|| Some(root_slug));
            let ctx = SearchContext { graph_state: &gs, embed_index: None, repo_root: &repo_root, lsp_status: None, root_filter, non_code_slugs: std::collections::HashSet::new() };
            let result = service::search(&params, &ctx).await;
            println!("{}", result);
            return Ok(());
        }
        Some(Commands::Graph(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            eprintln!("Scanning {}...", repo_root.display());
            let handler = RnaHandler { repo_root: repo_root.clone(), ..Default::default() };
            let gs = handler.build_full_graph().await?;
            let gp = GraphParams {
                node: args.node.clone(), mode: args.mode.clone(), direction: args.direction.clone(),
                edge_types: args.edge_types.as_ref().map(|s| s.split(',').map(|t| t.trim().to_string()).collect()),
                max_hops: args.max_hops,
            };
            match service::graph_query(&gp, &gs) {
                Ok(output) => println!("{}", output),
                Err(msg) => { eprintln!("Error: {}", msg); std::process::exit(1); }
            }
            let freshness = server::format_freshness(gs.nodes.len(), gs.last_scan_completed_at, None);
            eprintln!("{}", freshness);
            return Ok(());
        }
        Some(Commands::Stats(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            let lance_path = repo_root.join(".oh").join(".cache").join("lance");
            if !lance_path.exists() { eprintln!("No index found. Run `repo-native-alignment scan --path .` first."); std::process::exit(1); }
            let gs = server::load_graph_from_lance(&repo_root).await?;
            let st = service::stats(&repo_root, &gs).await;
            println!("── Repo Stats ──────────────────────────");
            println!("  Symbols:    {}", st.node_count);
            println!("  Edges:      {}", st.edge_count);
            println!("  Embeddings: {}", if st.embeddings_available { "yes" } else { "no" });
            println!("  Languages:  {}", if st.languages.is_empty() { "none".to_string() } else { st.languages.join(", ") });
            println!("  Last scan:  {}", st.last_scan_age);
            println!("  .oh/:       {} artifacts ({} outcomes, {} signals, {} guardrails, {} metis)",
                st.artifact_count, st.outcome_count, st.signal_count, st.guardrail_count, st.metis_count);
            println!("───────────────────────────────────────────");
            return Ok(());
        }
        None => {}
    }

    // ── server mode (default when no subcommand given) ───────────────────────
    let repo_root = cli.repo.canonicalize()?;

    let handler = RnaHandler {
        repo_root: repo_root.clone(),
        ..Default::default()
    };

    match cli.transport.as_str() {
        "stdio" => {
            // Logging to stderr only — stdout is the MCP channel
            init_tracing("warn", log_path.as_deref());

            tracing::info!(
                "Starting RNA MCP server (stdio) for repo at {}",
                repo_root.display()
            );

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

            server.start().await.map_err(|e| anyhow::anyhow!("{:?}", e))?;
        }
        "http" => {
            init_tracing("info", log_path.as_deref());

            tracing::info!(
                "Starting RNA MCP server on {}:{} for repo at {}",
                cli.host,
                cli.port,
                repo_root.display()
            );

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

            server.start().await.map_err(|e| anyhow::anyhow!("{:?}", e))?;
        }
        other => {
            anyhow::bail!("Unknown transport: {}. Use 'stdio' or 'http'.", other);
        }
    }

    Ok(())
}
