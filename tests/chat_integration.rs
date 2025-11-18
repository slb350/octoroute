//! Integration tests for /chat endpoint

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::post,
};
use octoroute::{
    config::Config,
    handlers::{AppState, chat::ChatResponse},
};
use tower::ServiceExt;

/// Helper to create test app with AppState
fn create_test_app() -> Router {
    let config = Config::from_file("config.toml").expect("failed to load config");
    let state = AppState::new(config);

    Router::new()
        .route("/chat", post(octoroute::handlers::chat::handler))
        .with_state(state)
}

#[tokio::test]
async fn test_chat_endpoint_with_valid_request() {
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/chat")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"message": "Hello, world!"}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return 200 OK
    assert_eq!(response.status(), StatusCode::OK);

    // Verify response structure
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let chat_response: ChatResponse =
        serde_json::from_slice(&body).expect("response should be valid ChatResponse JSON");

    // Verify response fields
    assert!(
        !chat_response.content.is_empty(),
        "content should not be empty"
    );
    assert!(
        ["fast", "balanced", "deep"].contains(&chat_response.model_tier.as_str()),
        "model_tier should be one of fast/balanced/deep, got {}",
        chat_response.model_tier
    );
    assert!(
        !chat_response.model_name.is_empty(),
        "model_name should not be empty"
    );
}

#[tokio::test]
async fn test_chat_endpoint_with_empty_message() {
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/chat")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"message": ""}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return 400 Bad Request for validation error
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    assert!(
        body_str.contains("empty") || body_str.contains("cannot be empty"),
        "error message should mention empty message, got: {}",
        body_str
    );
}

#[tokio::test]
async fn test_chat_endpoint_with_whitespace_only_message() {
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/chat")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"message": "   \n\t  "}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return 400 Bad Request for validation error
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    assert!(
        body_str.contains("empty") || body_str.contains("whitespace"),
        "error message should mention empty/whitespace, got: {}",
        body_str
    );
}

#[tokio::test]
async fn test_chat_endpoint_with_invalid_json() {
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/chat")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"message": "test", invalid json}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return 400 Bad Request for malformed JSON
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_chat_endpoint_with_no_available_endpoints() {
    // Create config with empty fast tier to trigger "no available endpoints" error
    let config_str = r#"
[server]
host = "127.0.0.1"
port = 8080

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 2048

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
default_importance = "normal"
router_model = "balanced"
"#;

    let mut config: Config = toml::from_str(config_str).expect("failed to parse config");

    // Empty ALL tiers to ensure we hit "no available endpoints" for any routing decision
    config.models.fast.clear();
    config.models.balanced.clear();
    config.models.deep.clear();

    let state = AppState::new(config);
    let app = Router::new()
        .route("/chat", post(octoroute::handlers::chat::handler))
        .with_state(state);

    let request = Request::builder()
        .method("POST")
        .uri("/chat")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"message": "Hello!"}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should return 500 Internal Server Error when no endpoints available
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    assert!(
        body_str.contains("No available endpoints") || body_str.contains("RoutingFailed"),
        "error message should mention no available endpoints, got: {}",
        body_str
    );
}
