//! Integration tests for /v1/chat/completions endpoint (OpenAI-compatible)
//!
//! Tests the OpenAI-compatible chat completions endpoint with various request
//! configurations including model selection, validation, and response format.

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware,
    routing::post,
};
use octoroute::{config::Config, handlers::AppState, middleware::request_id_middleware};
use std::sync::Arc;
use tower::ServiceExt;

/// Create test-specific config
fn create_test_config() -> Config {
    let toml = r#"
[server]
host = "127.0.0.1"
port = 8080
request_timeout_seconds = 30

[[models.fast]]
name = "test-fast-model"
base_url = "http://localhost:9999/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "test-balanced-model"
base_url = "http://localhost:9998/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "test-deep-model"
base_url = "http://localhost:9997/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
default_importance = "normal"
router_tier = "balanced"
"#;
    toml::from_str(toml).expect("should parse TOML config")
}

/// Helper to create test app with the real handler
/// Note: Tests that use this will fail if they actually try to call model endpoints
/// since they're not running. Use for validation/parsing tests only.
fn create_test_app() -> Router {
    let config = Arc::new(create_test_config());
    let state = AppState::new(config).expect("AppState::new should succeed");

    Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware))
}

// -------------------------------------------------------------------------
// Request Validation Tests
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_completions_rejects_empty_messages() {
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"model": "fast", "messages": []}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Validation errors from custom deserializers return 422 Unprocessable Entity
    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "Empty messages array should return 422"
    );

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    assert!(
        body_str.contains("empty") || body_str.contains("messages"),
        "Error should mention empty messages, got: {}",
        body_str
    );
}

#[tokio::test]
async fn test_completions_rejects_empty_user_content() {
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": ""}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "Empty user content should return 422"
    );
}

#[tokio::test]
async fn test_completions_rejects_invalid_temperature() {
    let app = create_test_app();

    // Temperature > 2.0 is invalid
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "temperature": 3.0}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "Invalid temperature should return 422"
    );

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    assert!(
        body_str.contains("temperature"),
        "Error should mention temperature, got: {}",
        body_str
    );
}

#[tokio::test]
async fn test_completions_rejects_invalid_top_p() {
    let app = create_test_app();

    // top_p > 1.0 is invalid
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "top_p": 1.5}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "Invalid top_p should return 422"
    );
}

#[tokio::test]
async fn test_completions_rejects_invalid_json() {
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"model": "fast", messages: invalid}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Invalid JSON syntax returns 400 Bad Request
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "Invalid JSON should return 400"
    );
}

#[tokio::test]
async fn test_completions_rejects_missing_model() {
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"messages": [{"role": "user", "content": "Hello"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Missing required field returns 422 Unprocessable Entity
    assert_eq!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "Missing model should return 422"
    );
}

#[tokio::test]
async fn test_completions_rejects_unknown_model() {
    let app = create_test_app();

    // Specific model that doesn't exist in config
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "nonexistent-model", "messages": [{"role": "user", "content": "Hello"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Unknown specific model returns 400 Bad Request (routing error)
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "Unknown model should return 400"
    );

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    assert!(
        body_str.contains("not found") || body_str.contains("nonexistent"),
        "Error should mention model not found, got: {}",
        body_str
    );
}

// -------------------------------------------------------------------------
// Model Choice Tests
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_completions_accepts_tier_names() {
    let app = create_test_app();

    // Test that tier names are accepted (parsing only - actual routing would fail)
    for model in &[
        "auto", "fast", "balanced", "deep", "AUTO", "Fast", "BALANCED",
    ] {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"model": "{}", "messages": [{{"role": "user", "content": "Hello"}}]}}"#,
                model
            )))
            .unwrap();

        let response = app.clone().oneshot(request).await.unwrap();

        // Should not be a validation error (4xx) - routing/model errors are expected since
        // test endpoints aren't running
        assert!(
            !matches!(
                response.status(),
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            ),
            "Model '{}' should be accepted (not a validation error), got status: {}",
            model,
            response.status()
        );
    }
}

// -------------------------------------------------------------------------
// Message Format Tests
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_completions_accepts_all_roles() {
    let app = create_test_app();

    // System + user messages
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
                "model": "fast",
                "messages": [
                    {"role": "system", "content": "You are a helpful assistant."},
                    {"role": "user", "content": "Hello!"},
                    {"role": "assistant", "content": "Hi there!"},
                    {"role": "user", "content": "How are you?"}
                ]
            }"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should not be a validation error
    assert!(
        !matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "Multi-role conversation should be accepted, got status: {}",
        response.status()
    );
}

#[tokio::test]
async fn test_completions_allows_empty_assistant_content() {
    let app = create_test_app();

    // Assistant messages can have empty content (for function calls, etc.)
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
                "model": "fast",
                "messages": [
                    {"role": "user", "content": "Hello!"},
                    {"role": "assistant", "content": ""},
                    {"role": "user", "content": "Continue"}
                ]
            }"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should not be a validation error
    assert!(
        !matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "Empty assistant content should be accepted, got status: {}",
        response.status()
    );
}

// -------------------------------------------------------------------------
// Optional Parameters Tests
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_completions_accepts_optional_parameters() {
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
                "model": "fast",
                "messages": [{"role": "user", "content": "Hello"}],
                "temperature": 0.5,
                "max_tokens": 100,
                "top_p": 0.9,
                "stream": false
            }"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should not be a validation error
    assert!(
        !matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "Optional parameters should be accepted, got status: {}",
        response.status()
    );
}
