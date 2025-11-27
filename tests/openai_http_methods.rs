//! HTTP method verification tests for OpenAI-compatible endpoints.
//!
//! Tests that endpoints only accept the correct HTTP methods and reject
//! others with 405 Method Not Allowed.

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware,
    routing::{get, post},
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

/// Create test app with both completions and models endpoints
fn create_test_app() -> Router {
    let config = Arc::new(create_test_config());
    let state = AppState::new(config).expect("AppState::new should succeed");

    Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .route(
            "/v1/models",
            get(octoroute::handlers::openai::models::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware))
}

// -------------------------------------------------------------------------
// /v1/chat/completions Method Tests (should only accept POST)
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_completions_rejects_get_method() {
    let app = create_test_app();

    let request = Request::builder()
        .method("GET")
        .uri("/v1/chat/completions")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "GET to /v1/chat/completions should return 405"
    );
}

#[tokio::test]
async fn test_completions_rejects_put_method() {
    let app = create_test_app();

    let request = Request::builder()
        .method("PUT")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "PUT to /v1/chat/completions should return 405"
    );
}

#[tokio::test]
async fn test_completions_rejects_delete_method() {
    let app = create_test_app();

    let request = Request::builder()
        .method("DELETE")
        .uri("/v1/chat/completions")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "DELETE to /v1/chat/completions should return 405"
    );
}

#[tokio::test]
async fn test_completions_rejects_patch_method() {
    let app = create_test_app();

    let request = Request::builder()
        .method("PATCH")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "PATCH to /v1/chat/completions should return 405"
    );
}

// -------------------------------------------------------------------------
// /v1/models Method Tests (should only accept GET)
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_models_rejects_post_method() {
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/v1/models")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "POST to /v1/models should return 405"
    );
}

#[tokio::test]
async fn test_models_rejects_put_method() {
    let app = create_test_app();

    let request = Request::builder()
        .method("PUT")
        .uri("/v1/models")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "PUT to /v1/models should return 405"
    );
}

#[tokio::test]
async fn test_models_rejects_delete_method() {
    let app = create_test_app();

    let request = Request::builder()
        .method("DELETE")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "DELETE to /v1/models should return 405"
    );
}

// -------------------------------------------------------------------------
// Positive Tests (verify correct methods work)
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_completions_accepts_post_method() {
    let app = create_test_app();

    // POST should be accepted (validation may fail, but not 405)
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_ne!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "POST to /v1/chat/completions should NOT return 405"
    );
}

#[tokio::test]
async fn test_models_accepts_get_method() {
    let app = create_test_app();

    // GET should return 200 OK with model list
    let request = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET to /v1/models should return 200"
    );
}
