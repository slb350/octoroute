//! Octoroute v2 local-first HTTP gateway.

use clap::Parser;
use octoroute::{
    cli::{Cli, Command, generate_config_template},
    gateway::{
        config::{GatewayConfig, ProcessEnvironment},
        env::DotenvEnvironment,
        http::gateway_app,
        service::GatewayService,
    },
    telemetry,
};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    if let Some(Command::Config { output }) = cli.command {
        return handle_config_command(output);
    }
    run_server(Path::new(&cli.config)).await
}

fn handle_config_command(output: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let template = generate_config_template();
    match output {
        Some(path) => {
            let path = PathBuf::from(path);
            if path.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("configuration file `{}` already exists", path.display()),
                )
                .into());
            }
            std::fs::write(&path, template)?;
            eprintln!("Octoroute v2 configuration written to {}", path.display());
            eprintln!(
                "Add OCTOROUTE_API_KEY and OPENROUTER_API_KEY to {} or the process environment.",
                path.parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(".env")
                    .display()
            );
        }
        None => print!("{template}"),
    }
    Ok(())
}

async fn run_server(config_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let input = std::fs::read_to_string(config_path)?;
    let dotenv_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".env");
    let environment = DotenvEnvironment::from_optional_path(&dotenv_path, ProcessEnvironment)?;
    let config = GatewayConfig::from_toml(&input, &environment)?;
    telemetry::init(config.observability().log_level().as_str());

    let address = SocketAddr::from((config.server().host(), config.server().port()));
    let service = GatewayService::from_config(config)?;
    let app = gateway_app(service);

    tracing::info!(%address, "starting Octoroute v2 gateway");
    tracing::info!("POST /v1/chat/completions");
    tracing::info!("GET /v1/models");
    tracing::info!("GET /health/live, /health/ready, /health");
    tracing::info!("GET /metrics");

    let listener = tokio::net::TcpListener::bind(address).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("Octoroute shutdown complete");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT"),
        _ = terminate => tracing::info!("received SIGTERM"),
    }
}
