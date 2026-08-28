//! Octoroute v3 inference-fabric executable.

use clap::Parser;
use octoroute::{
    cli::{Cli, Command, generate_config_template},
    gateway::{
        env::{DotenvEnvironment, ProcessEnvironment},
        fabric::{FabricConfig, FabricGatewayService, fabric_gateway_app},
    },
    telemetry,
};
use std::{io::Write, net::SocketAddr, path::Path};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Config { output }) => handle_config_command(output),
        None => run_server(Path::new(&cli.config)).await,
    };
    // Every startup failure carries a written `#[error]` message. Returning the
    // error from `main` would print its `Debug` form instead and make all of
    // them unreachable from the CLI.
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("octoroute: {error}");
            let mut source = error.source();
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            std::process::ExitCode::FAILURE
        }
    }
}

fn handle_config_command(output: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let template = generate_config_template();
    match output {
        Some(path) => {
            let path = Path::new(&path);
            write_new_artifact(path, "configuration file", template.as_bytes())?;
            eprintln!("Octoroute v3 configuration written to {}", path.display());
            eprintln!(
                "Add OCTOROUTE_API_KEY and credentials for enabled providers to {} or the process environment.",
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

fn write_new_artifact(path: &Path, label: &str, contents: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                std::io::Error::new(
                    error.kind(),
                    format!("{label} `{}` already exists", path.display()),
                )
            } else {
                error
            }
        })?;
    file.write_all(contents)
}

async fn run_server(config_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let input = std::fs::read_to_string(config_path)?;
    let config = FabricConfig::from_toml(&input)?;
    let address = SocketAddr::from((config.server.host, config.server.port));
    // Telemetry starts before service construction so warnings emitted while
    // building pools and the provider registry are not discarded.
    telemetry::init(&config.observability.log_level);
    let dotenv_path = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".env");
    let environment = DotenvEnvironment::from_optional_path(&dotenv_path, ProcessEnvironment)?;
    let service = FabricGatewayService::from_config(config, environment)?;
    let app = fabric_gateway_app(service);

    tracing::info!(%address, config_version = 3, "starting Octoroute gateway");
    tracing::info!("POST /v1/chat/completions");
    tracing::info!("GET /v1/models");
    tracing::info!("GET /health/live, /health/ready, /health");
    tracing::info!("GET /metrics");

    let listener = tokio::net::TcpListener::bind(address).await?;
    let serve = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal());
    // Graceful shutdown waits for in-flight responses, and an upstream deadline
    // can be 30 minutes. Without a bound, one long generation holds the process
    // past any service-manager stop timeout and it is killed anyway - after the
    // supervisor has stopped waiting, which is the worst of both.
    match tokio::time::timeout(SHUTDOWN_GRACE, serve).await {
        Ok(result) => result?,
        Err(_) => tracing::warn!(
            timeout_s = SHUTDOWN_GRACE.as_secs(),
            "graceful shutdown deadline elapsed with responses still in flight"
        ),
    }
    tracing::info!("Octoroute shutdown complete");
    Ok(())
}

/// Longest the gateway waits for in-flight responses after a stop signal.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

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
