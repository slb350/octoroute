//! Octoroute HTTP server
//!
//! Starts an Axum web server that routes LLM requests to optimal model endpoints.

use axum::{
    Router, middleware,
    routing::{get, post},
};
use clap::Parser;
use octoroute::{
    cli::{Cli, Command, generate_config_template},
    config::Config,
    error::AppError,
    handlers::{self, AppState},
    middleware::request_id_middleware,
    telemetry,
};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Handle subcommands
    if let Some(command) = cli.command {
        match command {
            Command::Config { output } => {
                return handle_config_command(output).map_err(|e| e.into());
            }
        }
    }

    // No subcommand - start the server
    run_server(&cli.config).await
}

/// Handle the `config` subcommand - generate template configuration
///
/// Generates a template configuration file that operators can customize for their setup.
///
/// # Behavior
///
/// - **No output argument**: Prints template to stdout (suitable for piping/viewing)
/// - **With output argument**: Writes to specified file with overwrite protection
///
/// # Errors
///
/// Returns `AppError::ConfigFileExists` if output file already exists.
/// Returns `AppError::ConfigFileWrite` if file write fails (permissions, disk space, etc.).
fn handle_config_command(output: Option<String>) -> Result<(), AppError> {
    let template = generate_config_template();

    match output {
        Some(path) => {
            // Check if file already exists (overwrite protection)
            if std::path::Path::new(&path).exists() {
                return Err(AppError::ConfigFileExists { path });
            }

            // Write template with contextual error handling
            std::fs::write(&path, template).map_err(|source| {
                let remediation = match source.kind() {
                    std::io::ErrorKind::PermissionDenied => format!(
                        "\nPermission denied. Check that:\n\
                        1. Parent directory has write permissions\n\
                        2. Current user can write to: {}",
                        std::path::Path::new(&path)
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| ".".to_string())
                    ),
                    std::io::ErrorKind::NotFound => format!(
                        "\nDirectory not found. Check that parent directory exists: {}",
                        std::path::Path::new(&path)
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| ".".to_string())
                    ),
                    _ => String::new(),
                };
                AppError::ConfigFileWrite {
                    path: path.clone(),
                    source,
                    remediation,
                }
            })?;

            eprintln!("Configuration template written to: {}", path);
            eprintln!(
                "Edit the file to configure your model endpoints, then run: octoroute --config {}",
                path
            );
        }
        None => {
            // Print to stdout
            print!("{}", template);
        }
    }

    Ok(())
}

/// Run the Octoroute server
async fn run_server(config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Load configuration
    let config = Config::from_file(config_path)?;

    // Initialize telemetry
    telemetry::init(&config.observability.log_level);

    tracing::info!(
        "Starting Octoroute server on {}:{}",
        config.server.host,
        config.server.port
    );

    // Wrap config in Arc before creating application state
    // This avoids unnecessary cloning - AppState accepts Arc<Config>
    let config = std::sync::Arc::new(config);

    // Create application state (fails if router construction fails)
    let state = AppState::new(config.clone())?;

    // Clone state for shutdown handler (state is moved to router)
    let shutdown_state = state.clone();

    // Build router with state and middleware
    let app = Router::new()
        // Legacy endpoints
        .route("/health", get(handlers::health::handler))
        .route("/chat", post(handlers::chat::handler))
        .route("/models", get(handlers::models::handler))
        .route("/metrics", get(handlers::metrics::handler))
        // OpenAI-compatible endpoints
        .route(
            "/v1/chat/completions",
            post(handlers::openai::completions::handler),
        )
        .route("/v1/models", get(handlers::openai::models::handler))
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    // Create socket address
    let ip_addr = config
        .server
        .host
        .parse::<std::net::IpAddr>()
        .map_err(|e| {
            format!(
                "Invalid IP address '{}' in config: {}. Expected format: 0.0.0.0 or 127.0.0.1",
                config.server.host, e
            )
        })?;

    let addr = SocketAddr::from((ip_addr, config.server.port));

    tracing::info!("Listening on {}", addr);
    tracing::info!("Health check available at http://{}/health", addr);
    tracing::info!("Legacy chat endpoint at http://{}/chat", addr);
    tracing::info!("Legacy models status at http://{}/models", addr);
    tracing::info!("Metrics endpoint at http://{}/metrics", addr);
    tracing::info!("OpenAI-compatible endpoints:");
    tracing::info!("  POST http://{}/v1/chat/completions", addr);
    tracing::info!("  GET  http://{}/v1/models", addr);

    // Start server with graceful shutdown
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_state))
        .await?;

    tracing::info!("Server shutdown complete");

    Ok(())
}

/// Wait for SIGTERM or SIGINT signal for graceful shutdown
///
/// Cancels background health checking task when shutdown signal is received.
async fn shutdown_signal(state: AppState) {
    use tokio::signal;

    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Received SIGINT (Ctrl+C), starting graceful shutdown");
        },
        _ = terminate => {
            tracing::info!("Received SIGTERM, starting graceful shutdown");
        },
    }

    // Cancel background health checking task
    state.selector().health_checker().shutdown().await;
}
