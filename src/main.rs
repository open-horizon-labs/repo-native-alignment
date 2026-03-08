use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use rust_mcp_sdk::McpServer;
use rust_mcp_sdk::schema::{Implementation, InitializeResult, ServerCapabilities};
use rust_mcp_sdk::ToMcpServerHandler;

use repo_native_alignment::server::RnaHandler;
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
}

#[derive(clap::Args, Debug)]
struct ScanArgs {
    /// Repository root path to scan (default: current directory or --repo)
    #[arg(long)]
    path: Option<PathBuf>,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // ── subcommands ──────────────────────────────────────────────────────────
    match cli.command {
        Some(Commands::Setup(args)) => return setup::run(&args),
        Some(Commands::Test(args)) => {
            let passed = smoke::run(&args).await?;
            std::process::exit(if passed { 0 } else { 1 });
        }
        Some(Commands::Scan(args)) => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .with_writer(std::io::stderr)
                .init();

            let repo_root = args
                .path
                .unwrap_or_else(|| cli.repo.clone())
                .canonicalize()?;

            eprintln!("Scanning: {}", repo_root.display());
            let t0 = std::time::Instant::now();

            let handler = RnaHandler {
                repo_root: repo_root.clone(),
                ..Default::default()
            };

            let graph = handler.build_full_graph().await?;
            let elapsed = t0.elapsed();

            let symbol_count = graph.nodes.len();
            let edge_count = graph.edges.len();
            let has_embeds = graph.embed_index.is_some();

            eprintln!();
            eprintln!("── Scan complete ──────────────────────────");
            eprintln!("  Symbols:    {}", symbol_count);
            eprintln!("  Edges:      {}", edge_count);
            eprintln!("  Embeddings: {}", if has_embeds { "yes" } else { "no" });
            eprintln!("  Time:       {:.2}s", elapsed.as_secs_f64());
            eprintln!("───────────────────────────────────────────");

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
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "warn".into()),
                )
                .with_writer(std::io::stderr)
                .init();

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
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .with_writer(std::io::stderr)
                .init();

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
