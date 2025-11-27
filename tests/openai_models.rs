//! Integration tests for /v1/models endpoint (OpenAI-compatible)
//!
//! Tests the OpenAI-compatible models list endpoint.

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::get,
};
use octoroute::{config::Config, handlers::AppState};
use serde::Deserialize;
use std::sync::Arc;
use tower::ServiceExt;

/// OpenAI model object response
#[derive(Debug, Deserialize)]
struct ModelObject {
    id: String,
    object: String,
    #[allow(dead_code)]
    created: i64,
    owned_by: String,
}

/// OpenAI models list response
#[derive(Debug, Deserialize)]
struct ModelsListResponse {
    object: String,
    data: Vec<ModelObject>,
}

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

/// Helper to create test app
fn create_test_app() -> Router {
    let config = Arc::new(create_test_config());
    let state = AppState::new(config).expect("AppState::new should succeed");

    Router::new()
        .route(
            "/v1/models",
            get(octoroute::handlers::openai::models::handler),
        )
        .with_state(state)
}

#[tokio::test]
async fn test_models_returns_200() {
    let app = create_test_app();

    let request = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET /v1/models should return 200"
    );
}

#[tokio::test]
async fn test_models_returns_list_object() {
    let app = create_test_app();

    let request = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let models: ModelsListResponse =
        serde_json::from_slice(&body).expect("Response should be valid JSON");

    assert_eq!(models.object, "list", "Response object should be 'list'");
}

#[tokio::test]
async fn test_models_includes_tier_names() {
    let app = create_test_app();

    let request = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let models: ModelsListResponse =
        serde_json::from_slice(&body).expect("Response should be valid JSON");

    let model_ids: Vec<&str> = models.data.iter().map(|m| m.id.as_str()).collect();

    // Should include tier-based virtual models
    assert!(
        model_ids.contains(&"auto"),
        "Should include 'auto' model, got: {:?}",
        model_ids
    );
    assert!(
        model_ids.contains(&"fast"),
        "Should include 'fast' model, got: {:?}",
        model_ids
    );
    assert!(
        model_ids.contains(&"balanced"),
        "Should include 'balanced' model, got: {:?}",
        model_ids
    );
    assert!(
        model_ids.contains(&"deep"),
        "Should include 'deep' model, got: {:?}",
        model_ids
    );
}

#[tokio::test]
async fn test_models_includes_configured_endpoints() {
    let app = create_test_app();

    let request = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let models: ModelsListResponse =
        serde_json::from_slice(&body).expect("Response should be valid JSON");

    let model_ids: Vec<&str> = models.data.iter().map(|m| m.id.as_str()).collect();

    // Should include configured endpoint names
    assert!(
        model_ids.contains(&"test-fast-model"),
        "Should include 'test-fast-model', got: {:?}",
        model_ids
    );
    assert!(
        model_ids.contains(&"test-balanced-model"),
        "Should include 'test-balanced-model', got: {:?}",
        model_ids
    );
    assert!(
        model_ids.contains(&"test-deep-model"),
        "Should include 'test-deep-model', got: {:?}",
        model_ids
    );
}

#[tokio::test]
async fn test_models_correct_ownership() {
    let app = create_test_app();

    let request = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let models: ModelsListResponse =
        serde_json::from_slice(&body).expect("Response should be valid JSON");

    // Tier models should be owned by "octoroute"
    for tier in &["auto", "fast", "balanced", "deep"] {
        let model = models.data.iter().find(|m| m.id == *tier);
        assert!(model.is_some(), "Should have model '{}'", tier);
        assert_eq!(
            model.unwrap().owned_by,
            "octoroute",
            "Tier model '{}' should be owned by 'octoroute'",
            tier
        );
    }

    // Configured endpoints should be owned by "user"
    for endpoint in &["test-fast-model", "test-balanced-model", "test-deep-model"] {
        let model = models.data.iter().find(|m| m.id == *endpoint);
        assert!(model.is_some(), "Should have model '{}'", endpoint);
        assert_eq!(
            model.unwrap().owned_by,
            "user",
            "Endpoint '{}' should be owned by 'user'",
            endpoint
        );
    }
}

#[tokio::test]
async fn test_models_object_type() {
    let app = create_test_app();

    let request = Request::builder()
        .method("GET")
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let models: ModelsListResponse =
        serde_json::from_slice(&body).expect("Response should be valid JSON");

    // All models should have object: "model"
    for model in &models.data {
        assert_eq!(
            model.object, "model",
            "Model '{}' should have object type 'model'",
            model.id
        );
    }
}
