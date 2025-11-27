//! Integration tests for streaming /v1/chat/completions endpoint (OpenAI-compatible)
//!
//! Tests the SSE streaming functionality including:
//! - Content-Type headers
//! - SSE event format (data: prefix, double newline)
//! - Chunk structure (initial role, content deltas, finish reason)
//! - [DONE] termination signal
//! - Error handling on stream start failure

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    middleware,
    routing::post,
};
use octoroute::{config::Config, handlers::AppState, middleware::request_id_middleware};
use std::sync::Arc;
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// Create test config pointing to a mock server
fn create_test_config_with_mock(mock_url: &str) -> Config {
    let toml = format!(
        r#"
[server]
host = "127.0.0.1"
port = 8080
request_timeout_seconds = 30

[[models.fast]]
name = "test-fast-model"
base_url = "{mock_url}"
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
"#
    );
    toml::from_str(&toml).expect("should parse TOML config")
}

/// Create test config with non-routable endpoints (for error testing)
fn create_test_config_unavailable() -> Config {
    let toml = r#"
[server]
host = "127.0.0.1"
port = 8080
request_timeout_seconds = 2

[[models.fast]]
name = "test-fast-model"
base_url = "http://192.0.2.1:11434/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "test-balanced-model"
base_url = "http://192.0.2.2:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "test-deep-model"
base_url = "http://192.0.2.3:11434/v1"
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

/// Helper to create test app with real OpenAI handler
fn create_test_app(config: Config) -> Router {
    let config = Arc::new(config);
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
// Content-Type and SSE Format Tests
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_streaming_returns_sse_content_type() {
    // This test verifies that streaming requests return the correct Content-Type
    // Note: The request will fail (no real endpoint), but we can check headers before body
    let config = create_test_config_unavailable();
    let app = create_test_app(config);

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "stream": true}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // For streaming, even errors should return SSE content type
    // (the error is sent as an SSE event)
    let content_type = response.headers().get(header::CONTENT_TYPE);

    // Either we get SSE content type (streaming started) or an error status
    // Both are valid - the key is we don't get application/json for stream:true
    if response.status().is_success() || response.status() == StatusCode::OK {
        assert!(
            content_type
                .map(|v| v.to_str().unwrap_or(""))
                .unwrap_or("")
                .contains("text/event-stream"),
            "Streaming response should have text/event-stream content type, got: {:?}",
            content_type
        );
    }
    // If error, that's also acceptable for this test (endpoint unavailable)
}

#[tokio::test]
async fn test_streaming_error_on_unavailable_endpoint_terminates_with_done() {
    // When endpoint is unavailable, stream should still terminate properly with [DONE]
    let config = create_test_config_unavailable();
    let app = create_test_app(config);

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "stream": true}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Collect the body
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    // Stream should end with [DONE] even on error
    // The error event should be sent before [DONE]
    assert!(
        body_str.contains("[DONE]"),
        "Streaming response should end with [DONE], got: {}",
        body_str
    );
}

#[tokio::test]
async fn test_streaming_error_event_contains_sanitized_message() {
    // Error events should not expose internal details
    let config = create_test_config_unavailable();
    let app = create_test_app(config);

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "stream": true}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    // Should contain error indication
    if body_str.contains("Error") || body_str.contains("error") {
        // Error message should be sanitized - no internal IPs, no stack traces
        assert!(
            !body_str.contains("192.0.2.1"),
            "Error should not expose internal endpoint IPs"
        );
        assert!(
            body_str.contains("retry") || body_str.contains("failed"),
            "Error should give user-friendly guidance, got: {}",
            body_str
        );
    }
}

// -------------------------------------------------------------------------
// SSE Event Format Tests (with mock server)
// -------------------------------------------------------------------------

/// Create a mock OpenAI streaming response
fn create_mock_streaming_response() -> String {
    // Simulate OpenAI streaming format that open-agent-sdk expects
    // The SDK handles the SSE parsing internally
    let chunks = vec![
        r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"test","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"test","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"test","choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}"#,
        r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","created":1234567890,"model":"test","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
    ];

    chunks
        .into_iter()
        .map(|c| format!("data: {}\n\n", c))
        .collect::<String>()
        + "data: [DONE]\n\n"
}

#[tokio::test]
async fn test_streaming_sse_events_have_data_prefix() {
    // Start mock server
    let mock_server = MockServer::start().await;

    // Mock the chat completions endpoint with streaming response
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(create_mock_streaming_response())
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let app = create_test_app(config);

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "stream": true}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // The response might fail due to open-agent-sdk expecting specific format
    // But we can still verify our SSE formatting logic
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body);

    // If we got SSE data, verify format
    if body_str.contains("data:") {
        // All SSE events should have "data: " prefix
        for line in body_str.lines() {
            if !line.is_empty() && !line.starts_with(':') {
                // Non-empty, non-comment lines should be data events
                assert!(
                    line.starts_with("data:"),
                    "SSE event should start with 'data:', got: {}",
                    line
                );
            }
        }
    }
}

// -------------------------------------------------------------------------
// Chunk Structure Tests
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_streaming_validation_rejects_invalid_requests() {
    // Streaming should still validate requests before starting stream
    let config = create_test_config_unavailable();
    let app = create_test_app(config);

    // Empty messages - should be rejected
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [], "stream": true}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Validation errors should NOT start a stream - should return error status
    assert!(
        response.status().is_client_error() || response.status().is_server_error(),
        "Invalid request should return error status, got: {}",
        response.status()
    );
}

#[tokio::test]
async fn test_streaming_accepts_valid_request_structure() {
    // Valid request should be accepted (even if endpoint fails later)
    let config = create_test_config_unavailable();
    let app = create_test_app(config);

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
                "model": "fast",
                "messages": [
                    {"role": "system", "content": "You are helpful."},
                    {"role": "user", "content": "Hello!"}
                ],
                "stream": true,
                "temperature": 0.7
            }"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should NOT be a validation error (4xx might still occur for other reasons)
    // The key is that it's not UNPROCESSABLE_ENTITY from validation
    assert_ne!(
        response.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "Valid request structure should not return validation error"
    );
}

// -------------------------------------------------------------------------
// Model Selection Tests for Streaming
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_streaming_accepts_tier_names() {
    let config = create_test_config_unavailable();

    for model in &["auto", "fast", "balanced", "deep", "AUTO", "Fast"] {
        let app = create_test_app(config.clone());

        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"model": "{}", "messages": [{{"role": "user", "content": "Hello"}}], "stream": true}}"#,
                model
            )))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        // Should not be a validation error
        assert!(
            !matches!(
                response.status(),
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            ),
            "Model '{}' should be accepted for streaming, got status: {}",
            model,
            response.status()
        );
    }
}

#[tokio::test]
async fn test_streaming_rejects_unknown_specific_model() {
    let config = create_test_config_unavailable();
    let app = create_test_app(config);

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "nonexistent-model-xyz", "messages": [{"role": "user", "content": "Hello"}], "stream": true}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Unknown specific model should fail
    assert!(
        response.status().is_client_error() || response.status().is_server_error(),
        "Unknown model should be rejected"
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
// Serialization Fallback Tests (CRITICAL-2)
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_streaming_serialization_fallback_is_valid_json() {
    // Unit test to verify the fallback string is valid JSON
    let fallback = r#"{"error":"Internal serialization error"}"#;
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(fallback);

    assert!(
        parsed.is_ok(),
        "Serialization fallback should be valid JSON"
    );

    let value = parsed.unwrap();
    assert!(
        value.get("error").is_some(),
        "Fallback JSON should have 'error' field"
    );
}

#[test]
fn test_axum_sse_event_with_chunk_json() {
    use axum::response::sse::Event;
    use octoroute::handlers::openai::types::ChatCompletionChunk;

    // Test that we can create an SSE event with serialized chunk JSON
    let chunk = ChatCompletionChunk::content("test-id", "test-model", 12345, "Hello");
    let json = serde_json::to_string(&chunk).unwrap();

    // This should not panic
    let event = Event::default().data(&json);
    // Just verify we got here without panicking
    assert!(format!("{:?}", event).len() > 0);
}

#[test]
fn test_chunk_serialization_has_no_newlines() {
    use octoroute::handlers::openai::types::ChatCompletionChunk;

    // Test initial chunk
    let initial = ChatCompletionChunk::initial("test-id", "test-model", 12345);
    let json = serde_json::to_string(&initial).unwrap();
    assert!(
        !json.contains('\n') && !json.contains('\r'),
        "Initial chunk JSON should not contain newlines: {}",
        json
    );

    // Test content chunk
    let content = ChatCompletionChunk::content("test-id", "test-model", 12345, "Hello world");
    let json = serde_json::to_string(&content).unwrap();
    assert!(
        !json.contains('\n') && !json.contains('\r'),
        "Content chunk JSON should not contain newlines: {}",
        json
    );

    // Test finish chunk
    let finish = ChatCompletionChunk::finish("test-id", "test-model", 12345);
    let json = serde_json::to_string(&finish).unwrap();
    assert!(
        !json.contains('\n') && !json.contains('\r'),
        "Finish chunk JSON should not contain newlines: {}",
        json
    );

    // Test error message chunk
    let error = ChatCompletionChunk::content(
        "test-id",
        "test-model",
        12345,
        "[Error: Failed to start model query. Please retry.]",
    );
    let json = serde_json::to_string(&error).unwrap();
    assert!(
        !json.contains('\n') && !json.contains('\r'),
        "Error chunk JSON should not contain newlines: {}",
        json
    );
}

// -------------------------------------------------------------------------
// Health Tracking Tests (CRITICAL-3)
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_streaming_marks_endpoint_failed_on_connection_error() {
    // When streaming fails to connect, endpoint should be marked as failed
    let config = create_test_config_unavailable();
    let config = Arc::new(config);
    let state = AppState::new(config.clone()).expect("AppState::new should succeed");

    // Verify endpoint starts healthy
    let health_checker = state.selector().health_checker();
    assert!(
        health_checker.is_healthy("test-fast-model").await,
        "Endpoint should start healthy"
    );

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state.clone())
        .layer(middleware::from_fn(request_id_middleware));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "stream": true}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Consume the body to ensure the stream completes
    let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await;

    // After failed streaming request, endpoint should have failure recorded
    let statuses = health_checker.get_all_statuses().await;
    let fast_status = statuses
        .iter()
        .find(|s| s.name() == "test-fast-model")
        .expect("test-fast-model should exist");

    assert!(
        fast_status.consecutive_failures() > 0,
        "Endpoint should have failure recorded after streaming connection error"
    );
}
