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
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// Create test-specific config with default unavailable endpoints
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

// -------------------------------------------------------------------------
// Specific Model Routing Tests (with mock server)
// -------------------------------------------------------------------------

/// Create config with a specific endpoint pointing to mock server
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

/// Verify that specific model routing bypasses tier selection and uses exact endpoint
///
/// When model="test-fast-model" (an endpoint name, not a tier), the request
/// should go directly to that endpoint without routing logic.
///
/// This test verifies routing by checking that the mock server received the request.
/// Note: The open_agent SDK expects streaming SSE format, so we can't easily mock
/// the full response flow. Instead, we verify the request reached the correct endpoint.
#[tokio::test]
async fn test_completions_specific_model_routes_to_exact_endpoint() {
    // Start mock server
    let mock_server = MockServer::start().await;

    // Create a mock that accepts the request - response format doesn't matter for routing test
    // The open_agent SDK will fail to parse this, but we can verify the request was routed
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            // Minimal streaming format that open_agent might accept
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: [DONE]\n\n",
        ))
        .expect(1) // Key assertion: exactly one request should hit this mock
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let config = Arc::new(config);
    let state = AppState::new(config).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    // Request using specific model name (not tier)
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "test-fast-model", "messages": [{"role": "user", "content": "Hello"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Consume response body (may be error due to SDK format mismatch, but routing is verified)
    let _body = axum::body::to_bytes(response.into_body(), usize::MAX).await;

    // The key verification is the expect(1) on the mock - wiremock will panic in drop
    // if the mock wasn't hit exactly once, proving the specific model was routed correctly
    //
    // Note: We can't verify response content reliably because open_agent SDK has
    // specific format requirements. The routing correctness is proven by the mock hit.
}

/// Verify that tier-based model selection uses routing (selects from tier)
///
/// When model="fast" (a tier name, not a specific endpoint), the request goes
/// through the tier selection logic which picks an endpoint from that tier.
#[tokio::test]
async fn test_completions_tier_model_uses_routing() {
    // Start mock server for the fast tier
    let mock_server = MockServer::start().await;

    // Create a mock that accepts requests
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: [DONE]\n\n",
        ))
        .expect(1) // Key assertion: request should be routed to the fast tier endpoint
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let config = Arc::new(config);
    let state = AppState::new(config).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    // Request using tier name "fast" (not specific endpoint name)
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Consume response body
    let _body = axum::body::to_bytes(response.into_body(), usize::MAX).await;

    // expect(1) verifies the mock was hit, proving tier routing selected the endpoint
}

// -------------------------------------------------------------------------
// Retry Endpoint Exclusion Tests
// -------------------------------------------------------------------------

/// Create config with two endpoints in the fast tier for retry testing
fn create_test_config_with_two_fast_endpoints(fast1_url: &str, fast2_url: &str) -> Config {
    let toml = format!(
        r#"
[server]
host = "127.0.0.1"
port = 8080
request_timeout_seconds = 30

[[models.fast]]
name = "fast-primary"
base_url = "{fast1_url}"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.fast]]
name = "fast-backup"
base_url = "{fast2_url}"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 2

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

/// Verify that failed endpoints are excluded from retry attempts
///
/// When an endpoint fails, the retry logic should:
/// 1. Mark the endpoint as failed in the exclusion set
/// 2. Select a different endpoint for the next attempt
/// 3. NOT retry the failed endpoint again within the same request
///
/// This test uses two mock servers:
/// - Primary endpoint: returns 500 error (simulating failure)
/// - Backup endpoint: returns success
///
/// If retry exclusion works correctly:
/// - Primary should receive exactly 1 request (fails, gets excluded)
/// - Backup should receive exactly 1 request (succeeds)
#[tokio::test]
async fn test_completions_retry_excludes_failed_endpoint() {
    // Start two mock servers - one for primary (will fail), one for backup (will succeed)
    let primary_mock = MockServer::start().await;
    let backup_mock = MockServer::start().await;

    // Primary endpoint returns 500 error - this simulates a failing endpoint
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .expect(1) // Should be hit exactly once, then excluded from retries
        .mount(&primary_mock)
        .await;

    // Backup endpoint returns success
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Success from backup\"}}]}\n\ndata: [DONE]\n\n",
        ))
        .expect(1) // Should be hit exactly once on retry
        .mount(&backup_mock)
        .await;

    let config =
        create_test_config_with_two_fast_endpoints(&primary_mock.uri(), &backup_mock.uri());
    let config = Arc::new(config);
    let state = AppState::new(config).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    // Request to the fast tier - primary will fail, retry should go to backup
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Consume response body
    let _body = axum::body::to_bytes(response.into_body(), usize::MAX).await;

    // The key verification is the expect(1) on both mocks:
    // - primary_mock.expect(1): proves it was tried exactly once, then excluded
    // - backup_mock.expect(1): proves retry went to backup, not back to primary
    //
    // If retry exclusion was broken, we'd see either:
    // - primary hit multiple times (retrying failed endpoint)
    // - backup not hit (never got to retry)
    //
    // wiremock panics in drop if expectations aren't met, so test passing = correct behavior
}

// -------------------------------------------------------------------------
// X-Octoroute-Warning Header Tests (HIGH-1)
// -------------------------------------------------------------------------

/// Test that the warning header constant is valid
#[test]
fn test_warning_header_constant_is_valid() {
    use octoroute::handlers::openai::X_OCTOROUTE_WARNING;

    // Header name must be lowercase (HTTP/2 requirement) and valid
    assert_eq!(X_OCTOROUTE_WARNING, "x-octoroute-warning");
    assert!(
        X_OCTOROUTE_WARNING
            .chars()
            .all(|c| c.is_ascii_lowercase() || c == '-')
    );
}

/// Test that successful response without warnings has no warning header
#[tokio::test]
async fn test_successful_response_has_no_warning_header() {
    use octoroute::handlers::openai::X_OCTOROUTE_WARNING;

    let mock_server = MockServer::start().await;

    // Create a valid response
    let response_json = r#"{
        "id": "chatcmpl-123",
        "object": "chat.completion",
        "created": 1677652288,
        "model": "test-fast-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Hello!"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 9, "completion_tokens": 12, "total_tokens": 21}
    }"#;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(response_json))
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let state = AppState::new(Arc::new(config)).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Successful response should not have the warning header
    assert!(
        response.headers().get(X_OCTOROUTE_WARNING).is_none(),
        "Successful response without issues should not have X-Octoroute-Warning header"
    );
}

// NOTE: Testing the warning header WITH health tracking failures requires mocking
// the health checker internals, which is complex. The implementation has been
// verified through code review and manual testing. A future improvement would be
// to add a test that injects a failing health checker via dependency injection.

// -------------------------------------------------------------------------
// Non-Streaming Success Path Response Structure Tests
// -------------------------------------------------------------------------

/// Helper to create an SSE-formatted response that open_agent SDK can parse
///
/// The SDK expects OpenAI-compatible SSE chunks with:
/// - `id`, `object`, `created`, `model` fields
/// - `choices[].delta.content` for text content
/// - `choices[].finish_reason` to signal completion ("stop")
fn create_sse_response(content: &str) -> String {
    let mut response = String::new();

    // Initial role chunk (required by SDK to establish assistant role)
    response.push_str(
        r#"data: {"id":"chatcmpl-test","object":"chat.completion.chunk","created":1234567890,"model":"test-model","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#,
    );
    response.push_str("\n\n");

    // Content chunks (split into smaller pieces for realism)
    for chunk in content.chars().collect::<Vec<_>>().chunks(10) {
        let chunk_str: String = chunk.iter().collect();
        // Escape quotes and backslashes in JSON
        let escaped = chunk_str
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        response.push_str(&format!(
            r#"data: {{"id":"chatcmpl-test","object":"chat.completion.chunk","created":1234567890,"model":"test-model","choices":[{{"index":0,"delta":{{"content":"{}"}},"finish_reason":null}}]}}"#,
            escaped
        ));
        response.push_str("\n\n");
    }

    // Finish chunk with finish_reason - SDK requires this to emit ContentBlock
    response.push_str(
        r#"data: {"id":"chatcmpl-test","object":"chat.completion.chunk","created":1234567890,"model":"test-model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
    );
    response.push_str("\n\n");

    // Done marker
    response.push_str("data: [DONE]\n\n");

    response
}

/// Test that non-streaming response returns valid ChatCompletion structure
#[tokio::test]
async fn test_non_streaming_response_has_valid_structure() {
    let mock_server = MockServer::start().await;

    let sse_response = create_sse_response("Hello, I'm an AI assistant!");

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(sse_response)
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let state = AppState::new(Arc::new(config)).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "stream": false}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body_bytes);

    assert_eq!(
        status,
        StatusCode::OK,
        "Non-streaming request should succeed. Got: {} - {}",
        status,
        body_str
    );

    // Parse the response as JSON
    let response_json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("Response should be valid JSON");

    // Verify all required fields exist
    assert!(
        response_json.get("id").is_some(),
        "Response must have 'id' field"
    );
    assert!(
        response_json.get("object").is_some(),
        "Response must have 'object' field"
    );
    assert!(
        response_json.get("created").is_some(),
        "Response must have 'created' field"
    );
    assert!(
        response_json.get("model").is_some(),
        "Response must have 'model' field"
    );
    assert!(
        response_json.get("choices").is_some(),
        "Response must have 'choices' field"
    );
    assert!(
        response_json.get("usage").is_some(),
        "Response must have 'usage' field"
    );
}

/// Test that response 'object' field is "chat.completion"
#[tokio::test]
async fn test_non_streaming_object_field_is_chat_completion() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(create_sse_response("Test response"))
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let state = AppState::new(Arc::new(config)).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hi"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let response_json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("Response should be valid JSON");

    assert_eq!(
        response_json["object"].as_str(),
        Some("chat.completion"),
        "Response 'object' field must be 'chat.completion'"
    );
}

/// Test that response 'id' starts with "chatcmpl-"
#[tokio::test]
async fn test_non_streaming_id_has_correct_prefix() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(create_sse_response("Test"))
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let state = AppState::new(Arc::new(config)).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hi"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let response_json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("Response should be valid JSON");

    let id = response_json["id"].as_str().expect("id must be a string");
    assert!(
        id.starts_with("chatcmpl-"),
        "Response 'id' must start with 'chatcmpl-', got: {}",
        id
    );
}

/// Test that response 'choices' array has exactly one element
#[tokio::test]
async fn test_non_streaming_choices_has_one_element() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(create_sse_response("Response content"))
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let state = AppState::new(Arc::new(config)).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hi"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let response_json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("Response should be valid JSON");

    let choices = response_json["choices"]
        .as_array()
        .expect("choices must be an array");

    assert_eq!(
        choices.len(),
        1,
        "Response 'choices' must have exactly one element"
    );
}

/// Test that response choice has finish_reason "stop"
#[tokio::test]
async fn test_non_streaming_finish_reason_is_stop() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(create_sse_response("Done"))
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let state = AppState::new(Arc::new(config)).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hi"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let response_json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("Response should be valid JSON");

    assert_eq!(
        response_json["choices"][0]["finish_reason"].as_str(),
        Some("stop"),
        "Response choice finish_reason must be 'stop'"
    );
}

/// Test that response 'usage' contains valid token counts
#[tokio::test]
async fn test_non_streaming_usage_has_valid_token_counts() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(create_sse_response(
                    "A longer response to ensure token estimation",
                ))
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let state = AppState::new(Arc::new(config)).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Generate a longer response please"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let response_json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("Response should be valid JSON");

    let usage = &response_json["usage"];

    // All token counts must be non-negative integers
    assert!(
        usage["prompt_tokens"].is_u64(),
        "usage.prompt_tokens must be a non-negative integer"
    );
    assert!(
        usage["completion_tokens"].is_u64(),
        "usage.completion_tokens must be a non-negative integer"
    );
    assert!(
        usage["total_tokens"].is_u64(),
        "usage.total_tokens must be a non-negative integer"
    );

    // total_tokens must equal prompt_tokens + completion_tokens
    let prompt_tokens = usage["prompt_tokens"].as_u64().unwrap();
    let completion_tokens = usage["completion_tokens"].as_u64().unwrap();
    let total_tokens = usage["total_tokens"].as_u64().unwrap();

    assert_eq!(
        total_tokens,
        prompt_tokens + completion_tokens,
        "total_tokens ({}) must equal prompt_tokens ({}) + completion_tokens ({})",
        total_tokens,
        prompt_tokens,
        completion_tokens
    );
}

/// Test that response message has role "assistant"
#[tokio::test]
async fn test_non_streaming_message_role_is_assistant() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(create_sse_response("Hello!"))
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let state = AppState::new(Arc::new(config)).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hi"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let response_json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("Response should be valid JSON");

    assert_eq!(
        response_json["choices"][0]["message"]["role"].as_str(),
        Some("assistant"),
        "Response message role must be 'assistant'"
    );
}

/// Test that response 'created' is a valid Unix timestamp (recent)
#[tokio::test]
async fn test_non_streaming_created_is_valid_timestamp() {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(create_sse_response("Response"))
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&mock_server)
        .await;

    let config = create_test_config_with_mock(&mock_server.uri());
    let state = AppState::new(Arc::new(config)).expect("AppState::new should succeed");

    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(octoroute::handlers::openai::completions::handler),
        )
        .with_state(state)
        .layer(middleware::from_fn(request_id_middleware));

    let before_request = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hi"}]}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    let after_request = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let response_json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("Response should be valid JSON");

    let created = response_json["created"]
        .as_i64()
        .expect("created must be an integer");

    // Timestamp should be within the request window
    assert!(
        created >= before_request && created <= after_request,
        "Response 'created' ({}) should be between {} and {}",
        created,
        before_request,
        after_request
    );
}

// -------------------------------------------------------------------------
// OpenAI Error Response Format Tests
// -------------------------------------------------------------------------

/// Tests that validation errors return OpenAI-compatible error format.
///
/// OpenAI SDKs expect error responses with this structure:
/// ```json
/// {
///   "error": {
///     "message": "...",
///     "type": "invalid_request_error",
///     "param": null,
///     "code": null
///   }
/// }
/// ```
///
/// This ensures compatibility with LangChain, OpenAI SDK, and other clients.
#[tokio::test]
async fn test_error_response_matches_openai_format() {
    let app = create_test_app();

    // Send request with invalid temperature (>2.0) to trigger validation error
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "test"}], "temperature": 5.0}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should be a client error
    assert!(
        response.status().is_client_error(),
        "Invalid temperature should return client error status"
    );

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let error_json: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("Response should be valid JSON");

    // OpenAI format has nested "error" object
    assert!(
        error_json.get("error").is_some(),
        "Response should have 'error' field, got: {}",
        error_json
    );

    let error_obj = &error_json["error"];

    // Error should be an object, not a string
    assert!(
        error_obj.is_object(),
        "error field should be an object (OpenAI format), not a string. Got: {}",
        error_obj
    );

    // Must have 'message' field
    assert!(
        error_obj.get("message").is_some(),
        "Error object should have 'message' field, got: {}",
        error_obj
    );

    // Must have 'type' field
    assert!(
        error_obj.get("type").is_some(),
        "Error object should have 'type' field, got: {}",
        error_obj
    );

    // Message should be a non-empty string
    let message = error_obj["message"].as_str().unwrap();
    assert!(!message.is_empty(), "Error message should not be empty");
}

/// Tests that validation errors have correct error type
#[tokio::test]
async fn test_validation_error_type_is_invalid_request() {
    let app = create_test_app();

    // Empty messages array triggers validation error
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"model": "fast", "messages": []}"#))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let error_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    let error_type = error_json["error"]["type"]
        .as_str()
        .expect("error.type should be a string");

    assert_eq!(
        error_type, "invalid_request_error",
        "Validation errors should have type 'invalid_request_error'"
    );
}
