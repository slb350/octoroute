//! Boundary value tests for OpenAI-compatible request validation.
//!
//! Tests the exact boundary conditions for request parameters to ensure
//! validation rejects values just outside the valid range and accepts
//! values at the exact boundaries.

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

/// Create test-specific config with unavailable endpoints
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

/// Helper to make a request with a given parameter value
async fn make_request_with_param(param_json: &str) -> (StatusCode, String) {
    let app = create_test_app();

    let body = format!(
        r#"{{"model": "fast", "messages": [{{"role": "user", "content": "Hello"}}], {}}}"#,
        param_json
    );

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8_lossy(&body_bytes).to_string();

    (status, body_str)
}

/// Helper to check that a value is accepted (not a validation error)
async fn assert_value_accepted(param_json: &str, description: &str) {
    let (status, body) = make_request_with_param(param_json).await;

    // Value is accepted if we don't get a 400 or 422 (routing/model errors are OK)
    assert!(
        !matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "{} should be accepted. Got status: {}, body: {}",
        description,
        status,
        body
    );
}

/// Helper to check that a value is rejected with 422
async fn assert_value_rejected(param_json: &str, expected_error_term: &str, description: &str) {
    let (status, body) = make_request_with_param(param_json).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{} should return 422. Got status: {}, body: {}",
        description,
        status,
        body
    );

    assert!(
        body.to_lowercase()
            .contains(&expected_error_term.to_lowercase()),
        "{} error should mention '{}'. Got: {}",
        description,
        expected_error_term,
        body
    );
}

// -------------------------------------------------------------------------
// Temperature Boundary Tests [0.0, 2.0]
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_temperature_valid_at_lower_boundary() {
    // temperature = 0.0 is the minimum valid value
    assert_value_accepted(r#""temperature": 0.0"#, "temperature=0.0 (lower boundary)").await;
}

#[tokio::test]
async fn test_temperature_valid_at_upper_boundary() {
    // temperature = 2.0 is the maximum valid value
    assert_value_accepted(r#""temperature": 2.0"#, "temperature=2.0 (upper boundary)").await;
}

#[tokio::test]
async fn test_temperature_valid_mid_range() {
    // temperature = 1.0 is a typical mid-range value
    assert_value_accepted(r#""temperature": 1.0"#, "temperature=1.0 (mid-range)").await;
}

#[tokio::test]
async fn test_temperature_invalid_below_minimum() {
    // temperature = -0.001 is just below the valid range
    assert_value_rejected(
        r#""temperature": -0.001"#,
        "temperature",
        "temperature=-0.001 (below minimum)",
    )
    .await;
}

#[tokio::test]
async fn test_temperature_invalid_above_maximum() {
    // temperature = 2.001 is just above the valid range
    assert_value_rejected(
        r#""temperature": 2.001"#,
        "temperature",
        "temperature=2.001 (above maximum)",
    )
    .await;
}

#[tokio::test]
async fn test_temperature_invalid_nan() {
    // NaN is not a valid temperature
    // JSON doesn't support NaN directly, but some parsers handle "NaN" string
    // We test by using a value that serde_json won't parse as valid float
    // Actually, JSON spec doesn't support NaN, so this test verifies the behavior
    // when we get a non-numeric value
    let app = create_test_app();

    // Using JavaScript-style NaN isn't valid JSON, so we'll use a string
    // which should fail JSON parsing (400) rather than validation (422)
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "temperature": "NaN"}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // String "NaN" where float expected is a type error
    assert!(
        matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "temperature='NaN' (string) should be rejected"
    );
}

#[tokio::test]
async fn test_temperature_invalid_infinity() {
    // Infinity is not a valid temperature
    // JSON doesn't support Infinity, similar to NaN test
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "temperature": 1e309}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&body_bytes);

    // 1e309 overflows to infinity in IEEE 754
    // Should be rejected either as invalid JSON or as validation failure
    assert!(
        matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "temperature=1e309 (overflow to infinity) should be rejected. Got: {} - {}",
        status,
        body
    );
}

#[tokio::test]
async fn test_temperature_invalid_negative_infinity() {
    // -Infinity is not a valid temperature
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "temperature": -1e309}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();

    assert!(
        matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "temperature=-1e309 (overflow to -infinity) should be rejected. Got: {}",
        status
    );
}

// -------------------------------------------------------------------------
// top_p Boundary Tests (0.0, 1.0] - exclusive lower, inclusive upper
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_top_p_invalid_at_zero() {
    // top_p = 0.0 is invalid (exclusive lower bound)
    assert_value_rejected(
        r#""top_p": 0.0"#,
        "top_p",
        "top_p=0.0 (invalid - exclusive lower)",
    )
    .await;
}

#[tokio::test]
async fn test_top_p_valid_just_above_zero() {
    // top_p = 0.001 should be valid (just above exclusive lower bound)
    assert_value_accepted(r#""top_p": 0.001"#, "top_p=0.001 (just above lower bound)").await;
}

#[tokio::test]
async fn test_top_p_valid_at_upper_boundary() {
    // top_p = 1.0 is the maximum valid value (inclusive)
    assert_value_accepted(r#""top_p": 1.0"#, "top_p=1.0 (upper boundary)").await;
}

#[tokio::test]
async fn test_top_p_invalid_above_maximum() {
    // top_p = 1.001 is just above the valid range
    assert_value_rejected(r#""top_p": 1.001"#, "top_p", "top_p=1.001 (above maximum)").await;
}

#[tokio::test]
async fn test_top_p_valid_mid_range() {
    // top_p = 0.5 is a typical mid-range value
    assert_value_accepted(r#""top_p": 0.5"#, "top_p=0.5 (mid-range)").await;
}

#[tokio::test]
async fn test_top_p_invalid_negative() {
    // top_p = -0.5 is invalid (negative)
    assert_value_rejected(r#""top_p": -0.5"#, "top_p", "top_p=-0.5 (negative)").await;
}

// -------------------------------------------------------------------------
// presence_penalty Boundary Tests [-2.0, 2.0]
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_presence_penalty_valid_at_lower_boundary() {
    // presence_penalty = -2.0 is the minimum valid value
    assert_value_accepted(
        r#""presence_penalty": -2.0"#,
        "presence_penalty=-2.0 (lower boundary)",
    )
    .await;
}

#[tokio::test]
async fn test_presence_penalty_valid_at_upper_boundary() {
    // presence_penalty = 2.0 is the maximum valid value
    assert_value_accepted(
        r#""presence_penalty": 2.0"#,
        "presence_penalty=2.0 (upper boundary)",
    )
    .await;
}

#[tokio::test]
async fn test_presence_penalty_valid_at_zero() {
    // presence_penalty = 0.0 (default-like value)
    assert_value_accepted(r#""presence_penalty": 0.0"#, "presence_penalty=0.0").await;
}

#[tokio::test]
async fn test_presence_penalty_invalid_below_minimum() {
    // presence_penalty = -2.001 is just below the valid range
    assert_value_rejected(
        r#""presence_penalty": -2.001"#,
        "presence_penalty",
        "presence_penalty=-2.001 (below minimum)",
    )
    .await;
}

#[tokio::test]
async fn test_presence_penalty_invalid_above_maximum() {
    // presence_penalty = 2.001 is just above the valid range
    assert_value_rejected(
        r#""presence_penalty": 2.001"#,
        "presence_penalty",
        "presence_penalty=2.001 (above maximum)",
    )
    .await;
}

// -------------------------------------------------------------------------
// frequency_penalty Boundary Tests [-2.0, 2.0]
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_frequency_penalty_valid_at_lower_boundary() {
    // frequency_penalty = -2.0 is the minimum valid value
    assert_value_accepted(
        r#""frequency_penalty": -2.0"#,
        "frequency_penalty=-2.0 (lower boundary)",
    )
    .await;
}

#[tokio::test]
async fn test_frequency_penalty_valid_at_upper_boundary() {
    // frequency_penalty = 2.0 is the maximum valid value
    assert_value_accepted(
        r#""frequency_penalty": 2.0"#,
        "frequency_penalty=2.0 (upper boundary)",
    )
    .await;
}

#[tokio::test]
async fn test_frequency_penalty_valid_at_zero() {
    // frequency_penalty = 0.0 (default-like value)
    assert_value_accepted(r#""frequency_penalty": 0.0"#, "frequency_penalty=0.0").await;
}

#[tokio::test]
async fn test_frequency_penalty_invalid_below_minimum() {
    // frequency_penalty = -2.001 is just below the valid range
    assert_value_rejected(
        r#""frequency_penalty": -2.001"#,
        "frequency_penalty",
        "frequency_penalty=-2.001 (below minimum)",
    )
    .await;
}

#[tokio::test]
async fn test_frequency_penalty_invalid_above_maximum() {
    // frequency_penalty = 2.001 is just above the valid range
    assert_value_rejected(
        r#""frequency_penalty": 2.001"#,
        "frequency_penalty",
        "frequency_penalty=2.001 (above maximum)",
    )
    .await;
}

// -------------------------------------------------------------------------
// max_tokens Boundary Tests (> 0)
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_max_tokens_invalid_at_zero() {
    // max_tokens = 0 is invalid
    assert_value_rejected(
        r#""max_tokens": 0"#,
        "max_tokens",
        "max_tokens=0 (invalid - must be > 0)",
    )
    .await;
}

#[tokio::test]
async fn test_max_tokens_valid_at_one() {
    // max_tokens = 1 is the minimum valid value
    assert_value_accepted(r#""max_tokens": 1"#, "max_tokens=1 (minimum valid)").await;
}

#[tokio::test]
async fn test_max_tokens_valid_large_value() {
    // max_tokens = 100000 should be valid (no upper limit in validation)
    assert_value_accepted(r#""max_tokens": 100000"#, "max_tokens=100000 (large value)").await;
}

#[tokio::test]
async fn test_max_tokens_invalid_negative() {
    // max_tokens is u32, so negative values should fail JSON parsing
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"model": "fast", "messages": [{"role": "user", "content": "Hello"}], "max_tokens": -1}"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Negative value for u32 should fail parsing
    assert!(
        matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "max_tokens=-1 should be rejected"
    );
}

// -------------------------------------------------------------------------
// Combined Parameter Tests
// -------------------------------------------------------------------------

#[tokio::test]
async fn test_all_parameters_at_boundaries() {
    // Test with all parameters at their valid boundary values
    let app = create_test_app();

    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{
                "model": "fast",
                "messages": [{"role": "user", "content": "Hello"}],
                "temperature": 0.0,
                "top_p": 1.0,
                "presence_penalty": -2.0,
                "frequency_penalty": 2.0,
                "max_tokens": 1
            }"#,
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // All boundary values together should be accepted (not validation error)
    assert!(
        !matches!(
            response.status(),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
        ),
        "All parameters at boundaries should be accepted. Got: {}",
        response.status()
    );
}
