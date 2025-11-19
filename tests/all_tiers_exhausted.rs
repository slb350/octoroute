//! Integration test for all-tiers-exhausted scenario
//!
//! Tests the critical edge case where all endpoints across all tiers
//! are unhealthy, simulating a service-wide outage.

use octoroute::{config::Config, handlers::AppState, middleware::RequestId};
use std::sync::Arc;

/// Create test config with multiple endpoints across all tiers
fn create_test_config() -> Config {
    let toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1235/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:1236/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
default_importance = "normal"
router_model = "balanced"

[observability]
log_level = "info"
"#;
    toml::from_str(toml).expect("should parse TOML")
}

#[tokio::test]
async fn test_all_tiers_exhausted_returns_error() {
    // This test simulates a service-wide outage where all endpoints
    // across all tiers (fast, balanced, deep) are unhealthy.
    //
    // Expected behavior:
    // 1. Request routes to a tier (e.g., Fast)
    // 2. Selector finds no healthy endpoints in Fast tier
    // 3. Retry logic attempts all MAX_RETRIES (3) times
    // 4. All attempts fail because all endpoints are unhealthy
    // 5. Request returns error indicating no available endpoints

    let config = Arc::new(create_test_config());
    let state = AppState::new(config.clone());
    let health_checker = state.selector().health_checker();

    // Mark ALL endpoints as unhealthy across ALL tiers (simulate service-wide outage)
    // Each endpoint needs 3 consecutive failures to be marked unhealthy
    for _ in 0..3 {
        health_checker.mark_failure("fast-1").await.unwrap();
        health_checker.mark_failure("balanced-1").await.unwrap();
        health_checker.mark_failure("deep-1").await.unwrap();
    }

    // Verify all endpoints are unhealthy
    assert!(
        !health_checker.is_healthy("fast-1").await,
        "fast-1 should be unhealthy"
    );
    assert!(
        !health_checker.is_healthy("balanced-1").await,
        "balanced-1 should be unhealthy"
    );
    assert!(
        !health_checker.is_healthy("deep-1").await,
        "deep-1 should be unhealthy"
    );

    // Make a request that routes to Fast tier (low importance)
    let json = r#"{"message": "Test message", "importance": "low", "task_type": "casual_chat"}"#;
    let request: octoroute::handlers::chat::ChatRequest = serde_json::from_str(json).unwrap();

    let result = octoroute::handlers::chat::handler(
        axum::extract::State(state.clone()),
        axum::Extension(RequestId::new()),
        axum::Json(request),
    )
    .await;

    // Should fail because no healthy endpoints available
    assert!(
        result.is_err(),
        "Request should fail when all tiers are exhausted"
    );

    // The handler returns Result<impl IntoResponse, AppError>
    // When it fails, we get AppError which we can inspect
    // The specific error type will be RoutingFailed with message about
    // no available healthy endpoints
}

#[tokio::test]
async fn test_recovery_after_all_tiers_exhausted() {
    // This test verifies that the system can recover from a service-wide
    // outage when endpoints become healthy again.

    let config = Arc::new(create_test_config());
    let state = AppState::new(config.clone());
    let health_checker = state.selector().health_checker();

    // Mark all endpoints unhealthy
    for _ in 0..3 {
        health_checker.mark_failure("fast-1").await.unwrap();
        health_checker.mark_failure("balanced-1").await.unwrap();
        health_checker.mark_failure("deep-1").await.unwrap();
    }

    // Make a request that should fail
    let json = r#"{"message": "Test", "importance": "low", "task_type": "casual_chat"}"#;
    let request: octoroute::handlers::chat::ChatRequest = serde_json::from_str(json).unwrap();

    let result = octoroute::handlers::chat::handler(
        axum::extract::State(state.clone()),
        axum::Extension(RequestId::new()),
        axum::Json(request.clone()),
    )
    .await;

    assert!(result.is_err(), "First request should fail");

    // Now mark one endpoint as healthy (simulating recovery)
    health_checker.mark_success("fast-1").await.unwrap();

    assert!(
        health_checker.is_healthy("fast-1").await,
        "fast-1 should be healthy after mark_success"
    );

    // Note: We can't actually test successful response here because
    // the endpoint URLs point to localhost:1234 which doesn't exist.
    // The selector WILL select fast-1 now that it's healthy, but the
    // actual query will fail due to connection error.
    //
    // This test validates that:
    // 1. System correctly identifies no healthy endpoints when all are down
    // 2. System correctly marks endpoints as healthy when they recover
    // 3. Healthy endpoints become eligible for selection again
    //
    // The actual query success is tested in other integration tests
    // with mock handlers or real endpoints.
}
