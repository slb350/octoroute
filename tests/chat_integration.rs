//! Integration tests for /chat endpoint
//!
//! These tests use a mock handler to avoid calling real model endpoints,
//! ensuring tests are hermetic and don't depend on external services.

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::post,
};
use octoroute::{
    config::{
        Config, ModelEndpoint, ModelsConfig, ObservabilityConfig, RoutingConfig, RoutingStrategy,
        ServerConfig,
    },
    error::AppError,
    handlers::{
        AppState,
        chat::{ChatRequest, ChatResponse},
    },
    router::Importance,
};
use tower::ServiceExt;

/// Mock chat handler for testing that doesn't call real model endpoints
async fn mock_chat_handler(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Perform same validation as real handler
    request.validate()?;

    // Convert to metadata for routing (test routing logic)
    let metadata = request.to_metadata();

    // Use real router to test routing decisions
    let target = state.router().route(&metadata).unwrap_or_else(|| {
        use octoroute::router::TargetModel;
        TargetModel::Balanced
    });

    // Use real selector to test endpoint selection (with health filtering)
    let endpoint = state.selector().select(target).await.ok_or_else(|| {
        AppError::RoutingFailed(format!("No available endpoints for tier {:?}", target))
    })?;

    // Return mock response without calling real model
    // This tests validation, routing, selection, and response serialization
    let response = ChatResponse {
        content: "Mock response for testing".to_string(),
        model_tier: format!("{:?}", target).to_lowercase(),
        model_name: endpoint.name.clone(),
    };

    Ok(Json(response))
}

/// Create test-specific config that doesn't require external services
fn create_test_config() -> Config {
    Config {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            request_timeout_seconds: 30,
        },
        models: ModelsConfig {
            fast: vec![ModelEndpoint {
                name: "test-fast-1".to_string(),
                base_url: "http://localhost:9999/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 1.0,
                priority: 1,
            }],
            balanced: vec![ModelEndpoint {
                name: "test-balanced-1".to_string(),
                base_url: "http://localhost:9998/v1".to_string(),
                max_tokens: 4096,
                temperature: 0.7,
                weight: 1.0,
                priority: 1,
            }],
            deep: vec![ModelEndpoint {
                name: "test-deep-1".to_string(),
                base_url: "http://localhost:9997/v1".to_string(),
                max_tokens: 8192,
                temperature: 0.7,
                weight: 1.0,
                priority: 1,
            }],
        },
        routing: RoutingConfig {
            strategy: RoutingStrategy::Rule,
            default_importance: Importance::Normal,
            router_model: "balanced".to_string(),
        },
        observability: ObservabilityConfig::default(),
    }
}

/// Helper to create test app with mock handler
fn create_test_app() -> Router {
    let config = create_test_config();
    let state = AppState::new(config);

    Router::new()
        .route("/chat", post(mock_chat_handler))
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

    // Verify response fields (mock handler returns mock data)
    assert_eq!(
        chat_response.content, "Mock response for testing",
        "content should be mock response"
    );
    assert!(
        ["fast", "balanced", "deep"].contains(&chat_response.model_tier.as_str()),
        "model_tier should be one of fast/balanced/deep, got {}",
        chat_response.model_tier
    );
    assert!(
        chat_response.model_name.starts_with("test-"),
        "model_name should be test endpoint, got {}",
        chat_response.model_name
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
    // Create config with all empty tiers to trigger "no available endpoints" error
    let mut config = create_test_config();
    config.models.fast.clear();
    config.models.balanced.clear();
    config.models.deep.clear();

    let state = AppState::new(config);
    let app = Router::new()
        .route("/chat", post(mock_chat_handler))
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
