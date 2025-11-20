//! Octoroute HTTP server
//!
//! Starts an Axum web server that routes LLM requests to optimal model endpoints.

use axum::{
    Router, middleware,
    routing::{get, post},
};
use octoroute::{
    config::Config,
    handlers::{self, AppState},
    middleware::request_id_middleware,
    telemetry,
};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load configuration
    let config = Config::from_file("config.toml")?;

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
        .route("/health", get(handlers::health::handler))
        .route("/chat", post(handlers::chat::handler))
        .route("/models", get(handlers::models::handler))
        .route("/metrics", get(handlers::metrics::handler))
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    // Create socket address
    let ip_addr = config
        .server
        .host
        .parse::<std::net::IpAddr>()
        .map_err(|e| {
            format!(
                "Invalid IP address '{}' in config.toml: {}. Expected format: 0.0.0.0 or 127.0.0.1",
                config.server.host, e
            )
        })?;

    let addr = SocketAddr::from((ip_addr, config.server.port));

    tracing::info!("Listening on {}", addr);
    tracing::info!("Health check available at http://{}/health", addr);
    tracing::info!("Chat endpoint available at http://{}/chat", addr);
    tracing::info!("Models status available at http://{}/models", addr);
    tracing::info!("Metrics endpoint available at http://{}/metrics", addr);

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
