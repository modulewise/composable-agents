use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use composable_a2a_server::A2aService;
use composable_agents::AgentService;
use composable_otel::OtelService;
use composable_runtime::Runtime;
use composable_tools::ToolService;

#[derive(Parser)]
#[command(name = "a2a-server")]
#[command(about = "An A2A server for agents built as Wasm components")]
struct Cli {
    /// Component definition files (.toml) and standalone .wasm files
    definitions: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    let runtime = Runtime::builder()
        .from_paths(&cli.definitions)
        .with_service::<OtelService>()
        .with_service::<ToolService>()
        .with_service::<AgentService>()
        .with_service::<A2aService>()
        .build()
        .await?;

    runtime.run().await
}
