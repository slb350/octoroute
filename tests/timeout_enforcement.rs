//! Integration tests for timeout enforcement
//!
//! Tests that request timeouts are properly enforced during streaming

use octoroute::{config::Config, handlers::AppState, middleware::RequestId};
use std::sync::Arc;

/// Create test config with very short timeout
fn create_short_timeout_config() -> Config {
    // ModelEndpoint fields are private - use TOML deserialization
    let toml = r#"
[server]
host = "127.0.0.1"
port = 8080
request_timeout_seconds = 1

[[models.fast]]
name = "fast-timeout-test"
base_url = "http://192.0.2.1:11434/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://192.0.2.2:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://192.0.2.3:11434/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
default_importance = "normal"
router_model = "balanced"

[observability]
log_level = "debug"
metrics_enabled = false
metrics_port = 9090
"#;
    toml::from_str(toml).expect("should parse TOML config")
}

#[tokio::test]
async fn test_request_fails_within_timeout_period() {
    // This test verifies that requests complete (either success or failure)
    // within the timeout period and don't hang indefinitely.
    // Connection failures should happen quickly, not wait for full timeout.

    let config = Arc::new(create_short_timeout_config());
    let state = AppState::new(config.clone());

    // Create a request
    let json = r#"{"message": "Test message", "importance": "low", "task_type": "casual_chat"}"#;
    let request: octoroute::handlers::chat::ChatRequest = serde_json::from_str(json).unwrap();

    let start = std::time::Instant::now();

    // Make the request - should fail (no real endpoints)
    let result = octoroute::handlers::chat::handler(
        axum::extract::State(state.clone()),
        axum::Extension(RequestId::new()),
        axum::Json(request),
    )
    .await;

    let elapsed = start.elapsed();

    // Request should fail (no endpoints available)
    assert!(
        result.is_err(),
        "Request should fail when endpoints are unreachable"
    );

    // Should complete within reasonable time (not hang forever)
    // With 1 second timeout and 3 retries, should complete within a few seconds
    assert!(
        elapsed.as_secs() < 10,
        "Request should complete within timeout window, took {:?}",
        elapsed
    );
}

#[tokio::test]
async fn test_timeout_includes_connection_time() {
    // This test verifies that the timeout includes connection establishment time,
    // not just the streaming phase

    let config = Arc::new(create_short_timeout_config());
    let state = AppState::new(config.clone());

    // Use a blackhole IP that will cause connection timeout
    let json = r#"{"message": "Hi", "importance": "low", "task_type": "casual_chat"}"#;
    let request: octoroute::handlers::chat::ChatRequest = serde_json::from_str(json).unwrap();

    let start = std::time::Instant::now();

    let result = octoroute::handlers::chat::handler(
        axum::extract::State(state),
        axum::Extension(RequestId::new()),
        axum::Json(request),
    )
    .await;

    let elapsed = start.elapsed();

    assert!(result.is_err(), "Request should fail");

    // Should timeout relatively quickly, not wait forever for connection
    // With 3 retries and 1 second timeout each, should be under 5 seconds
    assert!(
        elapsed.as_secs() < 10,
        "Connection timeout should be enforced, took {:?}",
        elapsed
    );
}

#[tokio::test]
async fn test_failures_dont_hang_indefinitely() {
    // This test verifies that connection failures don't cause the request to hang
    // forever. With timeout enforcement, failures should be returned promptly.

    let config = Arc::new(create_short_timeout_config());
    let state = AppState::new(config.clone());

    let json = r#"{"message": "Long request", "importance": "low", "task_type": "casual_chat"}"#;
    let request: octoroute::handlers::chat::ChatRequest = serde_json::from_str(json).unwrap();

    let start = std::time::Instant::now();

    let result = octoroute::handlers::chat::handler(
        axum::extract::State(state),
        axum::Extension(RequestId::new()),
        axum::Json(request),
    )
    .await;

    let elapsed = start.elapsed();

    // Should get an error (connection failure or timeout)
    assert!(result.is_err(), "Request should fail");

    // Should not hang - should complete within a reasonable timeframe
    assert!(
        elapsed.as_secs() < 10,
        "Request should not hang indefinitely, took {:?}",
        elapsed
    );
}
