use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::{Implementation, InitializeResult, ServerCapabilities};
use rust_mcp_sdk::ToMcpServerHandler;

use repo_native_alignment::server::{self, RnaHandler};
use repo_native_alignment::service::{self, GraphParams, OutcomeProgressContext, OutcomeProgressParams, RepoMapContext, RepoMapParams, SearchContext, SearchParams};
use repo_native_alignment::setup::{self, SetupArgs};
use repo_native_alignment::smoke_test::{self, TestArgs};

#[derive(Parser, Debug)]
#[command(name = "repo-native-alignment", version, about = "Repo-Native Alignment MCP Server", long_about = None)]
struct Cli {
    #[command(subcommand)] command: Option<Commands>,
    #[arg(long, default_value = ".")] repo: PathBuf,
    #[arg(long, default_value = "stdio")] transport: String,
    #[arg(long, default_value = "127.0.0.1")] host: String,
    #[arg(long, default_value_t = 8382)] port: u16,
    #[arg(long)] log_path: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Setup(SetupArgs), Test(TestArgs), Scan(ScanArgs), Search(SearchArgs), Graph(GraphArgs), Stats(StatsArgs),
    /// Track progress on a business outcome
    OutcomeProgress(OutcomeProgressCli),
    /// List configured workspace roots
    ListRoots(ListRootsCli),
    /// Show a high-level repository map
    RepoMap(RepoMapCli),
}

#[derive(clap::Args, Debug)] struct StatsArgs { #[arg(long, default_value = ".")] repo: PathBuf }
#[derive(clap::Args, Debug)] struct ScanArgs { #[arg(long)] path: Option<PathBuf>, #[arg(long)] full: bool, #[arg(long, default_value = ".")] repo: PathBuf }
#[derive(clap::Args, Debug)]
struct SearchArgs {
    #[arg(default_value = "")] query: String, #[arg(long, default_value = ".")] repo: PathBuf,
    #[arg(long)] kind: Option<String>, #[arg(long)] language: Option<String>, #[arg(long)] file: Option<String>,
    #[arg(long, default_value_t = 20)] limit: usize, #[arg(long)] node: Option<String>, #[arg(long)] mode: Option<String>,
    #[arg(long)] hops: Option<u32>, #[arg(long)] direction: Option<String>, #[arg(long)] edge_types: Option<String>,
    #[arg(long)] sort_by: Option<String>, #[arg(long)] min_complexity: Option<u32>, #[arg(long)] synthetic: Option<bool>,
    #[arg(long)] compact: bool, #[arg(long)] nodes: Option<String>, #[arg(long)] search_mode: Option<String>,
    #[arg(long, default_value_t = true)] include_artifacts: bool, #[arg(long, default_value_t = true)] include_markdown: bool,
    #[arg(long)] artifact_types: Option<String>, #[arg(long)] root: Option<String>,
    #[arg(long)] rerank: bool,
}
#[derive(clap::Args, Debug)]
struct GraphArgs {
    #[arg(long)] node: String, #[arg(long, default_value = "neighbors")] mode: String,
    #[arg(long, default_value = "outgoing")] direction: String, #[arg(long)] edge_types: Option<String>,
    #[arg(long)] max_hops: Option<usize>, #[arg(long, default_value = ".")] repo: PathBuf,
}
#[derive(clap::Args, Debug)]
struct OutcomeProgressCli { outcome_id: String, #[arg(long)] include_impact: bool, #[arg(long)] root: Option<String>, #[arg(long, default_value = ".")] repo: PathBuf }
#[derive(clap::Args, Debug)]
struct ListRootsCli { #[arg(long, default_value = ".")] repo: PathBuf }
#[derive(clap::Args, Debug)]
struct RepoMapCli { #[arg(long, default_value_t = 15)] top_n: usize, #[arg(long)] root: Option<String>, #[arg(long, default_value = ".")] repo: PathBuf }

fn server_details() -> InitializeResult {
    InitializeResult { capabilities: ServerCapabilities { tools: Some(Default::default()), ..Default::default() },
        instructions: Some("Repo-Native Alignment: query business outcomes, code, markdown, and git history.".into()),
        meta: None, protocol_version: "2025-11-25".into(),
        server_info: Implementation { name: "rna-server".into(), version: env!("CARGO_PKG_VERSION").into(),
            description: Some("MCP server for querying business outcomes, code, and git history".into()),
            icons: vec![], title: Some("Repo-Native Alignment".into()), website_url: None } }
}

/// Baseline directives that suppress noisy library internals.
/// These are prepended to whatever default_filter the caller provides,
/// so caller-level directives (e.g. `info`) win for RNA's own crate while
/// lance internals stay at WARN unless RUST_LOG explicitly overrides.
const LIBRARY_NOISE_FILTER: &str = "lance=warn,lance_index=warn,lance_file=warn,lancedb=warn";

fn init_tracing(default_filter: &str, log_path: Option<&std::path::Path>) {
    use tracing_subscriber::prelude::*;
    let composite_default = format!("{},{}", LIBRARY_NOISE_FILTER, default_filter);
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| composite_default.into());
    let effective_log_path = log_path.map(|p| p.to_path_buf()).or_else(|| std::env::var("RNA_LOG_FILE").ok().map(PathBuf::from));
    if let Some(ref file_path) = effective_log_path {
        if let Some(parent) = file_path.parent() { let _ = std::fs::create_dir_all(parent); }
        if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(file_path) {
            let file_layer = tracing_subscriber::fmt::layer().with_writer(std::sync::Mutex::new(file)).with_ansi(false);
            let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
            tracing_subscriber::registry().with(env_filter).with(stderr_layer).with(file_layer).init();
            tracing::info!("Logging to file: {}", file_path.display()); return;
        }
    }
    tracing_subscriber::fmt().with_env_filter(env_filter).with_writer(std::io::stderr).init();
}

fn resolve_root_filter(root_arg: Option<&str>, repo_root: &std::path::Path) -> Option<String> {
    let root_slug = repo_native_alignment::roots::RootConfig::code_project(repo_root.to_path_buf()).slug();
    root_arg.map(|v| if v.eq_ignore_ascii_case("all") { None } else { Some(v.to_string()) }).unwrap_or_else(|| Some(root_slug))
}

fn main() -> anyhow::Result<()> {
    // Set fastembed model cache to ~/.cache/rna/models/ instead of .fastembed_cache/
    // in the current directory. Must be set before Tokio runtime and any fastembed
    // initialization (reranker model, or any future fastembed embedding model).
    if std::env::var("FASTEMBED_CACHE_DIR").ok().filter(|v| !v.is_empty()).is_none()
        && let Ok(home) = std::env::var("HOME")
    {
        let cache_dir = std::path::PathBuf::from(home).join(".cache").join("rna").join("models");
        // SAFETY: called in single-threaded main() before Tokio runtime starts.
        unsafe { std::env::set_var("FASTEMBED_CACHE_DIR", &cache_dir) };
    }

    async_main()
}

#[tokio::main]
async fn async_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let log_path = cli.log_path.clone();
    match cli.command {
        Some(Commands::Setup(args)) => return setup::run(&args),
        Some(Commands::Test(args)) => { init_tracing("info", log_path.as_deref()); let passed = smoke_test::run(&args).await?; std::process::exit(if passed { 0 } else { 1 }); }
        Some(Commands::Scan(args)) => {
            init_tracing("info", log_path.as_deref());
            let repo_root = args.path.unwrap_or_else(|| args.repo.clone()).canonicalize()?;
            if args.full { eprintln!("Full pipeline scan: {}", repo_root.display()); let handler = RnaHandler { repo_root: repo_root.clone(), ..Default::default() }; handler.run_pipeline_foreground(|msg| { eprintln!("{}", msg); }).await?; return Ok(()); }
            eprintln!("Scanning: {}", repo_root.display()); let t0 = std::time::Instant::now();
            let handler = RnaHandler { repo_root: repo_root.clone(), ..Default::default() }; let graph = handler.build_full_graph().await?;
            let embed_count = match repo_native_alignment::embed::EmbeddingIndex::new(&repo_root).await {
                Ok(idx) => { eprintln!("  Embedding {} symbols...", graph.nodes.iter().filter(|n| n.id.root != "external").count());
                    loop { match idx.search("_probe_", None, 1).await { Ok(repo_native_alignment::embed::SearchOutcome::Results(_)) => break 1, _ => { tokio::time::sleep(std::time::Duration::from_secs(2)).await } } } }
                Err(e) => { eprintln!("  Embedding init failed: {}", e); 0 }
            };
            let elapsed = t0.elapsed(); eprintln!(); eprintln!("  Symbols: {} | Edges: {} | Embeddings: {} | Time: {:.2}s", graph.nodes.len(), graph.edges.len(), if embed_count > 0 { "yes" } else { "no" }, elapsed.as_secs_f64());
            return Ok(());
        }
        Some(Commands::Search(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            // Load graph from LanceDB cache -- do NOT rebuild.
            // If cache doesn't exist, tell the user to run `rna scan`.
            let lance_path = repo_root.join(".oh").join(".cache").join("lance");
            let gs = if lance_path.exists() {
                match server::load_graph_from_lance(&repo_root).await {
                    Ok(state) => {
                        eprintln!("Loaded {} symbols from cache.", state.nodes.len());
                        state
                    }
                    Err(e) => {
                        eprintln!("Failed to load cached index: {}. Run `repo-native-alignment scan --path .` first.", e);
                        std::process::exit(1);
                    }
                }
            } else {
                eprintln!("No index found. Run `repo-native-alignment scan --path .` first.");
                std::process::exit(1);
            };
            // Load existing embedding index -- do NOT rebuild.
            let embed_idx = match repo_native_alignment::embed::EmbeddingIndex::new(&repo_root).await {
                Ok(idx) => {
                    match idx.has_table().await {
                        Ok(true) => Some(idx),
                        Ok(false) => {
                            eprintln!("No embedding index found. Run `repo-native-alignment scan --path .` first.");
                            None
                        }
                        Err(e) => {
                            eprintln!("Embedding index check failed: {}. Semantic search will be disabled.", e);
                            None
                        }
                    }
                }
                Err(e) => { tracing::warn!("EmbeddingIndex init failed; semantic search may degrade: {}", e); None }
            };
            let embed_ref = embed_idx.as_ref();
            let params = SearchParams {
                query: if args.query.is_empty() { None } else { Some(args.query.clone()) },
                node: args.node.clone(), mode: args.mode.clone(), hops: args.hops, direction: args.direction.clone(),
                edge_types: args.edge_types.as_ref().map(|s| s.split(',').map(|t| t.trim().to_string()).collect()),
                kind: args.kind.clone(), language: args.language.clone(), file: args.file.clone(),
                limit: Some(args.limit), sort_by: args.sort_by.clone(), min_complexity: args.min_complexity,
                synthetic: args.synthetic, compact: args.compact,
                nodes: args.nodes.as_ref().map(|s| s.split(',').map(|t| t.trim().to_string()).collect()),
                search_mode: args.search_mode.clone(), rerank: args.rerank,
                include_artifacts: args.include_artifacts, include_markdown: args.include_markdown,
                artifact_types: args.artifact_types.as_ref().map(|s| s.split(',').map(|t| t.trim().to_string()).collect()),
            };
            let root_filter = resolve_root_filter(args.root.as_deref(), &repo_root);
            let ctx = SearchContext { graph_state: &gs, embed_index: embed_ref, repo_root: &repo_root, lsp_status: None, embed_status: None, root_filter, non_code_slugs: std::collections::HashSet::new() };
            println!("{}", service::search(&params, &ctx).await); return Ok(());
        }
        Some(Commands::Graph(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?; eprintln!("Scanning {}...", repo_root.display());
            let handler = RnaHandler { repo_root: repo_root.clone(), ..Default::default() }; let gs = handler.build_full_graph().await?;
            let gp = GraphParams { node: args.node.clone(), mode: args.mode.clone(), direction: args.direction.clone(),
                edge_types: args.edge_types.as_ref().map(|s| s.split(',').map(|t| t.trim().to_string()).collect()), max_hops: args.max_hops };
            match service::graph_query(&gp, &gs) { Ok(output) => println!("{}", output), Err(msg) => { eprintln!("Error: {}", msg); std::process::exit(1); } }
            eprintln!("{}", server::format_freshness(gs.nodes.len(), gs.last_scan_completed_at, None)); return Ok(());
        }
        Some(Commands::Stats(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            let lance_path = repo_root.join(".oh").join(".cache").join("lance");
            if !lance_path.exists() { eprintln!("No index found. Run `repo-native-alignment scan --path .` first."); std::process::exit(1); }
            let gs = server::load_graph_from_lance(&repo_root).await?;
            let st = service::stats(&repo_root, &gs).await;
            println!("  Symbols: {} | Edges: {} | Embeddings: {} | Languages: {} | Last scan: {} | .oh/: {} artifacts ({} outcomes, {} signals, {} guardrails, {} metis)",
                st.node_count, st.edge_count, if st.embeddings_available { "yes" } else { "no" },
                if st.languages.is_empty() { "none".to_string() } else { st.languages.join(", ") }, st.last_scan_age,
                st.artifact_count, st.outcome_count, st.signal_count, st.guardrail_count, st.metis_count);
            return Ok(());
        }
        Some(Commands::OutcomeProgress(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?; eprintln!("Scanning {}...", repo_root.display());
            let handler = RnaHandler { repo_root: repo_root.clone(), ..Default::default() }; let gs = handler.build_full_graph().await?;
            let root_filter = resolve_root_filter(args.root.as_deref(), &repo_root);
            let params = OutcomeProgressParams { outcome_id: args.outcome_id.clone(), include_impact: args.include_impact, root_filter, non_code_slugs: std::collections::HashSet::new() };
            let ctx = OutcomeProgressContext { graph_state: &gs, repo_root: &repo_root };
            println!("{}", service::outcome_progress(&params, &ctx)); return Ok(());
        }
        Some(Commands::ListRoots(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?;
            println!("{}", service::list_roots(&repo_root)); return Ok(());
        }
        Some(Commands::RepoMap(args)) => {
            init_tracing("warn", log_path.as_deref());
            let repo_root = args.repo.canonicalize()?; eprintln!("Scanning {}...", repo_root.display());
            let handler = RnaHandler { repo_root: repo_root.clone(), ..Default::default() }; let gs = handler.build_full_graph().await?;
            let root_filter = resolve_root_filter(args.root.as_deref(), &repo_root);
            let params = RepoMapParams { top_n: args.top_n, root_filter, non_code_slugs: std::collections::HashSet::new() };
            let ctx = RepoMapContext { graph_state: &gs, repo_root: &repo_root, lsp_status: None, embed_status: None };
            println!("{}", service::repo_map(&params, &ctx)); return Ok(());
        }
        None => {}
    }
    let repo_root = cli.repo.canonicalize()?;
    let handler = RnaHandler { repo_root: repo_root.clone(), ..Default::default() };
    match cli.transport.as_str() {
        "stdio" => { init_tracing("warn", log_path.as_deref());
            let transport = rust_mcp_sdk::StdioTransport::new(Default::default()).map_err(|e| anyhow::anyhow!("{:?}", e))?;
            let server = rust_mcp_sdk::mcp_server::server_runtime::create_server(rust_mcp_sdk::mcp_server::McpServerOptions { server_details: server_details(), transport, handler: handler.to_mcp_server_handler(), task_store: None, client_task_store: None });
            server.start().await.map_err(|e| anyhow::anyhow!("{:?}", e))?; }
        "http" => { init_tracing("info", log_path.as_deref());
            let server = rust_mcp_sdk::mcp_server::hyper_server::create_server(server_details(), handler.to_mcp_server_handler(), rust_mcp_sdk::mcp_server::HyperServerOptions { host: cli.host, port: cli.port, event_store: Some(Arc::new(rust_mcp_sdk::event_store::InMemoryEventStore::default())), ..Default::default() });
            server.start().await.map_err(|e| anyhow::anyhow!("{:?}", e))?; }
        other => { anyhow::bail!("Unknown transport: {}. Use 'stdio' or 'http'.", other); }
    }
    Ok(())
}
