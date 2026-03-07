use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use rust_mcp_sdk::event_store::InMemoryEventStore;
use rust_mcp_sdk::mcp_server::HyperServerOptions;
use rust_mcp_sdk::schema::{Implementation, InitializeResult, ServerCapabilities};
use rust_mcp_sdk::ToMcpServerHandler;

use repo_native_alignment::server::RnaHandler;

#[derive(Parser, Debug)]
#[command(name = "rna-server", about = "Repo-Native Alignment MCP Server")]
struct Cli {
    /// Repository root path (default: current directory)
    #[arg(long, default_value = ".")]
    repo: PathBuf,

    /// Host to bind to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to bind to
    #[arg(long, default_value_t = 8382)]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let repo_root = cli.repo.canonicalize()?;

    tracing::info!(
        "Starting RNA MCP server on {}:{} for repo at {}",
        cli.host,
        cli.port,
        repo_root.display()
    );

    let handler = RnaHandler { repo_root };

    let server_details = InitializeResult {
        capabilities: ServerCapabilities {
            tools: Some(Default::default()),
            ..Default::default()
        },
        instructions: Some(
            "Repo-Native Alignment: query business outcomes, code, markdown, and git history."
                .to_string(),
        ),
        meta: None,
        protocol_version: "2025-03-26".to_string(),
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
    };

    let server = rust_mcp_sdk::mcp_server::hyper_server::create_server(
        server_details,
        handler.to_mcp_server_handler(),
        HyperServerOptions {
            host: cli.host,
            port: cli.port,
            event_store: Some(Arc::new(InMemoryEventStore::default())),
            ..Default::default()
        },
    );

    server.start().await.map_err(|e| anyhow::anyhow!("{:?}", e))?;
    Ok(())
}
