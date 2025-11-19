//! Integration test for router returning None
//!
//! Verifies that when the rule-based router returns None (no rule matches),
//! the chat handler correctly returns an error with appropriate status code
//! and error message.

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

fn create_test_config() -> Config {
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
strategy = "rule"
router_model = "balanced"
"#;
    toml::from_str(toml).expect("should parse TOML")
}

fn create_test_app() -> Router {
    use axum::middleware;
    use octoroute::middleware::request_id_middleware;

    let config = Arc::new(create_test_config());
    let state = AppState::new(config);

    Router::new()
        .route("/chat", post(octoroute::handlers::chat::handler))
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware))
}

#[tokio::test]
async fn test_router_returns_none_results_in_error() {
    // This test verifies that when the rule-based router returns None (no routing rule matches),
    // the hybrid router falls back to LLM routing. Since the test endpoints don't exist,
    // the LLM router will fail to connect and return an appropriate error.
    //
    // Scenario: Send a request with metadata that doesn't match any routing rule.
    // The rule-based router will return None, triggering LLM fallback.

    let app = create_test_app();

    // Create a request that won't match any routing rule
    // According to rule_based.rs tests, casual_chat + high importance returns None (ambiguous case)
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

    // With HybridRouter, when rule-based routing returns None, it falls back to LLM routing.
    // Since the test endpoints don't exist, the LLM router fails to connect to the balanced tier.
    // This results in 500 (Internal Server Error) or 502 (Bad Gateway).
    assert!(
        matches!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR | StatusCode::BAD_GATEWAY
        ),
        "should return 500 or 502 when LLM fallback fails to connect, got: {}",
        response.status()
    );

    // Verify error message indicates the issue
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    assert!(
        body_str.contains("routing")
            || body_str.contains("Routing")
            || body_str.contains("endpoints")
            || body_str.contains("Router query failed"),
        "error message should indicate routing or endpoint issue, got: {}",
        body_str
    );
}

#[tokio::test]
async fn test_router_none_includes_helpful_error_details() {
    // Verify that the error message includes helpful details for debugging
    // With HybridRouter, when rule-based routing fails, it falls back to LLM routing

    let app = create_test_app();

    let request_body = r#"{
        "message": "test",
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

    // With HybridRouter fallback, expect either 500 or 502
    assert!(
        matches!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR | StatusCode::BAD_GATEWAY
        ),
        "expected 500 or 502, got: {}",
        response.status()
    );

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    // Verify error message is informative
    // With LLM fallback, the error will mention endpoint connection or routing issues
    assert!(
        body_str.contains("Router query failed")
            || body_str.contains("endpoints")
            || body_str.contains("routing"),
        "error should include debugging information, got: {}",
        body_str
    );
}

#[tokio::test]
async fn test_router_with_matching_rule_succeeds() {
    // Positive test: Verify that requests with matching rules work correctly
    // This ensures the error path is only triggered when appropriate

    let app = create_test_app();

    // This should match Rule 1: Trivial/casual tasks → Fast tier
    let request_body = r#"{
        "message": "hi",
        "task_type": "casual_chat",
        "importance": "normal"
    }"#;

    let request = Request::builder()
        .method("POST")
        .uri("/chat")
        .header("content-type", "application/json")
        .body(Body::from(request_body))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Note: This will fail because we're not actually running a model endpoint,
    // but it should get past the routing stage. We expect a different error
    // (connection error, not routing error).
    //
    // If we get 500 with "routing" in the error, the router returned None (bug).
    // If we get 502/504 (bad gateway/timeout), routing succeeded but model connection failed (expected).

    let status = response.status();

    if status == StatusCode::INTERNAL_SERVER_ERROR {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);

        // If it's a routing error, the test should fail
        if body_str.contains("routing") || body_str.contains("Routing") {
            panic!(
                "Routing should have succeeded for CasualChat + Normal importance. \
                Got routing error: {}",
                body_str
            );
        }
    }

    // We expect BAD_GATEWAY (502) or GATEWAY_TIMEOUT (504) because the model endpoints don't exist
    // Or INTERNAL_SERVER_ERROR (500) if it's a non-routing error (e.g., health check issue)
    assert!(
        matches!(
            status,
            StatusCode::BAD_GATEWAY
                | StatusCode::GATEWAY_TIMEOUT
                | StatusCode::INTERNAL_SERVER_ERROR
        ),
        "expected 502, 504, or 500 (non-routing error), got: {}",
        status
    );
}
