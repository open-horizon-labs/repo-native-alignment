use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use petgraph::Direction;
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::{Implementation, InitializeResult, ServerCapabilities};
use rust_mcp_sdk::ToMcpServerHandler;

use repo_native_alignment::server::{self, RnaHandler};
use repo_native_alignment::setup::{self, SetupArgs};
use repo_native_alignment::smoke::{self, TestArgs};

#[derive(Parser, Debug)]
#[command(
    name = "repo-native-alignment",
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
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Bootstrap RNA + OH MCP setup for a project
    Setup(SetupArgs),
    /// Run the full pipeline smoke test (scan → extract → embed → index → query).
    ///
    /// Exits 0 on pass, 1 on any failure. Runnable in CI with no extra dependencies.
    Test(TestArgs),
    /// Scan, extract, embed, and persist the full graph for a repo.
    ///
    /// Runs the same pipeline as MCP server startup but standalone, with timing output.
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
}

#[derive(clap::Args, Debug)]
struct SearchArgs {
    /// Search query (matched against symbol name and signature)
    query: String,
    /// Repository root path (default: current directory)
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Filter by symbol kind: function, struct, trait, enum, module, import, const
    #[arg(long)]
    kind: Option<String>,
    /// Filter by language: rust, python, typescript, go, etc.
    #[arg(long)]
    language: Option<String>,
    /// Filter by file path substring
    #[arg(long)]
    file: Option<String>,
    /// Maximum results (default: 20)
    #[arg(long, default_value_t = 20)]
    limit: usize,
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

fn init_tracing(default_filter: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // ── subcommands ──────────────────────────────────────────────────────────
    match cli.command {
        Some(Commands::Setup(args)) => return setup::run(&args),
        Some(Commands::Test(args)) => {
            init_tracing("info");
            tracing::info!("Running RNA pipeline smoke test for {}", args.repo.display());
            let passed = smoke::run(&args).await?;
            std::process::exit(if passed { 0 } else { 1 });
        }
        Some(Commands::Scan(args)) => {
            init_tracing("info");
            let repo_root = args.path.unwrap_or_else(|| cli.repo.clone()).canonicalize()?;
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
            // build_full_graph spawns embedding as a background task.
            // Wait for it by polling the embedding index until it's populated.
            let embed_count = match repo_native_alignment::embed::EmbeddingIndex::new(&repo_root).await {
                Ok(idx) => {
                    eprintln!("  Embedding {} symbols...", graph.nodes.iter().filter(|n| n.id.root != "external").count());
                    loop {
                        match idx.search("_probe_", None, 1).await {
                            Ok(_) => break 1, // table exists and is queryable
                            Err(_) => tokio::time::sleep(std::time::Duration::from_secs(2)).await,
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
            init_tracing("warn");
            let repo_root = args.repo.canonicalize()?;
            eprintln!("Scanning {}...", repo_root.display());
            let handler = RnaHandler {
                repo_root: repo_root.clone(),
                ..Default::default()
            };
            let gs = handler.build_full_graph().await?;
            let query_lower = args.query.to_lowercase();

            let mut matches: Vec<_> = gs.nodes.iter()
                .filter(|n| {
                    let name_match = n.id.name.to_lowercase().contains(&query_lower)
                        || n.signature.to_lowercase().contains(&query_lower);
                    if !name_match { return false; }
                    if let Some(ref k) = args.kind {
                        if n.id.kind.to_string().to_lowercase() != k.to_lowercase() {
                            return false;
                        }
                    }
                    if let Some(ref l) = args.language {
                        if n.language.to_lowercase() != l.to_lowercase() {
                            return false;
                        }
                    }
                    if let Some(ref f) = args.file {
                        if !n.id.file.to_string_lossy().contains(f.as_str()) {
                            return false;
                        }
                    }
                    true
                })
                .collect();

            matches.truncate(args.limit);

            if matches.is_empty() {
                println!("No symbols matching \"{}\".", args.query);
            } else {
                println!("## Symbol search: \"{}\"\n\n{} result(s)\n", args.query, matches.len());
                for n in &matches {
                    let stable_id = n.stable_id();
                    let outgoing = gs.index.neighbors(&stable_id, None, Direction::Outgoing);
                    let incoming = gs.index.neighbors(&stable_id, None, Direction::Incoming);
                    println!(
                        "- **{}** `{}` ({}) `{}`:{}-{}",
                        n.id.kind, n.id.name, n.language,
                        n.id.file.display(), n.line_start, n.line_end,
                    );
                    println!("  ID: `{}`", stable_id);
                    if !n.signature.is_empty() {
                        println!("  Sig: `{}`", n.signature);
                    }
                    if let Some(val) = n.metadata.get("value") {
                        println!("  Value: `{}`", val);
                    }
                    if !outgoing.is_empty() {
                        println!("  Out: {} edge(s)", outgoing.len());
                    }
                    if !incoming.is_empty() {
                        println!("  In: {} edge(s)", incoming.len());
                    }
                    println!();
                }
            }
            let freshness = server::format_freshness(gs.nodes.len(), gs.last_scan_completed_at);
            eprintln!("{}", freshness);
            return Ok(());
        }
        Some(Commands::Graph(args)) => {
            init_tracing("warn");
            let repo_root = args.repo.canonicalize()?;
            eprintln!("Scanning {}...", repo_root.display());
            let handler = RnaHandler {
                repo_root: repo_root.clone(),
                ..Default::default()
            };
            let gs = handler.build_full_graph().await?;

            let edge_filter = args.edge_types.as_ref().map(|types| {
                types.split(',')
                    .filter_map(|t| server::parse_edge_kind(t.trim()))
                    .collect::<Vec<_>>()
            });
            let edge_filter_slice = edge_filter.as_deref();

            let result_ids = match args.mode.as_str() {
                "neighbors" => {
                    let max_hops = args.max_hops.unwrap_or(1);
                    match args.direction.as_str() {
                        "outgoing" => {
                            if max_hops == 1 {
                                gs.index.neighbors(&args.node, edge_filter_slice, Direction::Outgoing)
                            } else {
                                gs.index.reachable(&args.node, max_hops, edge_filter_slice)
                            }
                        }
                        "incoming" => {
                            if max_hops == 1 {
                                gs.index.neighbors(&args.node, edge_filter_slice, Direction::Incoming)
                            } else {
                                gs.index.impact(&args.node, max_hops)
                            }
                        }
                        "both" => {
                            let mut ids = if max_hops == 1 {
                                gs.index.neighbors(&args.node, edge_filter_slice, Direction::Outgoing)
                            } else {
                                gs.index.reachable(&args.node, max_hops, edge_filter_slice)
                            };
                            let inc = if max_hops == 1 {
                                gs.index.neighbors(&args.node, edge_filter_slice, Direction::Incoming)
                            } else {
                                gs.index.impact(&args.node, max_hops)
                            };
                            ids.extend(inc);
                            ids.sort();
                            ids.dedup();
                            ids
                        }
                        other => {
                            anyhow::bail!("Invalid direction: \"{}\". Use outgoing, incoming, or both.", other);
                        }
                    }
                }
                "impact" => {
                    let max_hops = args.max_hops.unwrap_or(3);
                    gs.index.impact(&args.node, max_hops)
                }
                "reachable" => {
                    let max_hops = args.max_hops.unwrap_or(3);
                    gs.index.reachable(&args.node, max_hops, edge_filter_slice)
                }
                other => {
                    anyhow::bail!("Invalid mode: \"{}\". Use neighbors, impact, or reachable.", other);
                }
            };

            if result_ids.is_empty() {
                println!("No results for `{}` ({}).", args.node, args.mode);
            } else {
                println!("## {} `{}`\n\n{} result(s)\n", args.mode, args.node, result_ids.len());
                for id in &result_ids {
                    if let Some(node) = gs.nodes.iter().find(|n| n.stable_id() == *id) {
                        // Filter out module and PR-merge noise
                        match node.id.kind {
                            repo_native_alignment::graph::NodeKind::Module
                            | repo_native_alignment::graph::NodeKind::PrMerge => continue,
                            _ => {}
                        }
                        println!(
                            "- **{}** `{}` ({}) `{}`:{}-{}",
                            node.id.kind, node.id.name, node.language,
                            node.id.file.display(), node.line_start, node.line_end,
                        );
                        if !node.signature.is_empty() {
                            println!("  Sig: `{}`", node.signature);
                        }
                    } else {
                        println!("- `{}`", id);
                    }
                }
            }
            let freshness = server::format_freshness(gs.nodes.len(), gs.last_scan_completed_at);
            eprintln!("{}", freshness);
            return Ok(());
        }
        Some(Commands::Stats(args)) => {
            init_tracing("warn");
            let repo_root = args.repo.canonicalize()?;

            let lance_path = repo_root.join(".oh").join(".cache").join("lance");
            if !lance_path.exists() {
                eprintln!("No index found. Run `repo-native-alignment scan --path .` first.");
                std::process::exit(1);
            }

            let last_scan = std::fs::metadata(&lance_path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
                .map(|d| {
                    let secs_ago = std::time::SystemTime::now()
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() - d.as_secs();
                    if secs_ago < 60 { "just now".to_string() }
                    else if secs_ago < 3600 { format!("{}m ago", secs_ago / 60) }
                    else if secs_ago < 86400 { format!("{}h ago", secs_ago / 3600) }
                    else { format!("{}d ago", secs_ago / 86400) }
                })
                .unwrap_or_else(|| "unknown".to_string());

            let gs = server::load_graph_from_lance(&repo_root).await?;

            let mut langs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for n in &gs.nodes {
                if !n.language.is_empty() && n.language != "unknown" {
                    langs.insert(n.language.clone());
                }
            }

            let artifacts = repo_native_alignment::oh::load_oh_artifacts(&repo_root).unwrap_or_default();
            let outcomes = artifacts.iter().filter(|a| a.kind == repo_native_alignment::types::OhArtifactKind::Outcome).count();
            let signals = artifacts.iter().filter(|a| a.kind == repo_native_alignment::types::OhArtifactKind::Signal).count();
            let guardrails = artifacts.iter().filter(|a| a.kind == repo_native_alignment::types::OhArtifactKind::Guardrail).count();
            let metis = artifacts.iter().filter(|a| a.kind == repo_native_alignment::types::OhArtifactKind::Metis).count();

            let embed_str = if gs.embed_index.is_some() { "yes" } else { "no" };

            println!("── Repo Stats ──────────────────────────");
            println!("  Symbols:    {}", gs.nodes.len());
            println!("  Edges:      {}", gs.edges.len());
            println!("  Embeddings: {}", embed_str);
            println!("  Languages:  {}", if langs.is_empty() { "none".to_string() } else { langs.into_iter().collect::<Vec<_>>().join(", ") });
            println!("  Last scan:  {}", last_scan);
            println!("  .oh/:       {} artifacts ({} outcomes, {} signals, {} guardrails, {} metis)",
                artifacts.len(), outcomes, signals, guardrails, metis);
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
            init_tracing("warn");

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
            init_tracing("info");

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
