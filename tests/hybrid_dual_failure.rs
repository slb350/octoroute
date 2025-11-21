//! Integration test for hybrid router dual-failure scenario
//!
//! Verifies behavior when BOTH routing strategies fail:
//! 1. Rule-based router returns None (no rule matches ambiguous case)
//! 2. LLM fallback fails because all balanced endpoints are unhealthy
//!
//! This test ensures the error message is clear and includes helpful context
//! about the balanced tier requirement for LLM routing.

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::post,
};
use octoroute::config::Config;
use octoroute::handlers::AppState;
use std::sync::Arc;
use tower::ServiceExt; // for `oneshot`

fn create_hybrid_config() -> Config {
    let toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
router_tier = "balanced"
"#;
    toml::from_str(toml).expect("should parse TOML")
}

#[tokio::test]
async fn test_chat_endpoint_hybrid_fallback_all_balanced_unhealthy() {
    // GAP #3: Hybrid Router Dual-Failure Integration Test
    //
    // Scenario:
    // 1. User sends ambiguous request (CasualChat + High importance)
    // 2. Rule-based router returns None (no rule matches)
    // 3. Hybrid router falls back to LLM routing
    // 4. All balanced endpoints are marked unhealthy
    // 5. LLM router fails (cannot select from balanced tier)
    //
    // Verifies:
    // - HTTP 500 Internal Server Error returned
    // - Error message mentions balanced tier or routing failure
    // - Error is informative for debugging

    let config = Arc::new(create_hybrid_config());
    let state = AppState::new(config).expect("AppState::new should succeed");

    // Mark all balanced endpoints unhealthy (3 consecutive failures)
    let health_checker = state.selector().health_checker();
    for _ in 0..3 {
        health_checker
            .mark_failure("balanced-1")
            .await
            .expect("mark_failure should succeed");
    }

    let app = Router::new()
        .route("/chat", post(octoroute::handlers::chat::handler))
        .with_state(state)
        .layer(axum::middleware::from_fn(
            octoroute::middleware::request_id_middleware,
        ));

    // Send ambiguous request that triggers LLM fallback
    // (CasualChat + High importance has no rule match in rule_based.rs)
    let request_body = r#"{
        "message": "test message",
        "task_type": "casual_chat",
        "importance": "high"
    }"#;

    let request = Request::builder()
        .method("POST")
        .uri("/chat")
        .header("content-type", "application/json")
        .body(Body::from(request_body))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Verify HTTP 500 (Internal Server Error)
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "Should return 500 when both rule and LLM routing fail"
    );

    // Verify error message is informative
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    // Error should mention routing failure or balanced tier
    assert!(
        body_str.to_lowercase().contains("balanced")
            || body_str.contains("routing")
            || body_str.contains("Router query failed")
            || body_str.contains("endpoints"),
        "Error message should indicate balanced tier or routing issue, got: {}",
        body_str
    );
}

#[tokio::test]
async fn test_hybrid_routing_succeeds_with_healthy_balanced() {
    // Positive control test: Verify hybrid routing works when balanced endpoints are healthy
    //
    // This ensures the dual-failure scenario only occurs when appropriate.
    // With healthy balanced endpoints, the LLM fallback should at least attempt to query
    // (even though the endpoint doesn't actually exist in the test environment).

    let config = Arc::new(create_hybrid_config());
    let state = AppState::new(config).expect("AppState::new should succeed");

    let app = Router::new()
        .route("/chat", post(octoroute::handlers::chat::handler))
        .with_state(state)
        .layer(axum::middleware::from_fn(
            octoroute::middleware::request_id_middleware,
        ));

    // Same ambiguous request as above
    let request_body = r#"{
        "message": "test message",
        "task_type": "casual_chat",
        "importance": "high"
    }"#;

    let request = Request::builder()
        .method("POST")
        .uri("/chat")
        .header("content-type", "application/json")
        .body(Body::from(request_body))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // With healthy balanced endpoints, we expect connection/timeout errors (502/504)
    // not routing errors (500 with "no healthy endpoints" message)
    assert!(
        matches!(
            response.status(),
            StatusCode::BAD_GATEWAY
                | StatusCode::GATEWAY_TIMEOUT
                | StatusCode::INTERNAL_SERVER_ERROR
        ),
        "Expected 502/504 or 500 (connection error), got: {}",
        response.status()
    );

    // If 500, verify it's NOT a routing error
    if response.status() == StatusCode::INTERNAL_SERVER_ERROR {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);

        // Should NOT be an "endpoints exhausted" or "no healthy" error
        // (those indicate routing failure, not connection failure)
        assert!(
            !body_str.contains("no healthy endpoints")
                && !body_str.contains("all endpoints unhealthy"),
            "Should not be routing error when balanced endpoints are healthy, got: {}",
            body_str
        );
    }
}
