//! Integration tests for /health endpoint

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::get,
};
use octoroute::{config::Config, handlers};
use std::sync::Arc;
use tower::ServiceExt; // for `oneshot` and `ready`

fn create_test_state() -> handlers::AppState {
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
router_tier = "balanced"
"#;
    let config: Config = toml::from_str(toml).expect("should parse test config");
    handlers::AppState::new(Arc::new(config)).expect("should create AppState")
}

#[tokio::test]
async fn test_health_endpoint_returns_ok() {
    let state = create_test_state();
    let app = Router::new()
        .route("/health", get(handlers::health::handler))
        .with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("should parse JSON");

    assert_eq!(json["status"], "OK");
    assert_eq!(json["health_tracking_status"], "operational");
}

#[tokio::test]
async fn test_health_endpoint_not_found() {
    let state = create_test_state();
    let app = Router::new()
        .route("/health", get(handlers::health::handler))
        .with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
