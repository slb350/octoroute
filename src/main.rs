//! Octoroute HTTP server
//!
//! Starts an Axum web server that routes LLM requests to optimal model endpoints.

use axum::{Router, routing::get};
use octoroute::{config::Config, handlers, telemetry};
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

    // Build router
    let app = Router::new().route("/health", get(handlers::health::handler));

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

    // Start server
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
