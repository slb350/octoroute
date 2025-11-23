//! Tests for error context preservation
//!
//! Verifies that error types are preserved through the error chain rather than
//! being converted to strings, which loses type information and makes debugging harder.
//!
//! Addresses PR #4 Issue: HealthError converted to string, loses type info

use octoroute::config::Config;
use octoroute::error::AppError;
use octoroute::handlers::AppState;
use std::sync::Arc;

/// Helper to create a test config
fn create_test_config() -> Config {
    let config_toml = r#"
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
"#;

    toml::from_str(config_toml).expect("should parse test config")
}

/// Test that HealthError type is preserved in AppError
///
/// **RED PHASE**: This test will fail because AppError::HealthTracking doesn't exist yet
#[tokio::test]
async fn test_health_error_type_preserved() {
    let config = Arc::new(create_test_config());
    let state = AppState::new(config).expect("should create AppState");

    // Try to mark success on a nonexistent endpoint
    let result = state
        .selector()
        .health_checker()
        .mark_success("nonexistent-endpoint")
        .await;

    assert!(result.is_err(), "Should fail for unknown endpoint");

    // The error should be a HealthError::UnknownEndpoint
    let err = result.unwrap_err();

    // Verify it's the UnknownEndpoint variant
    match err {
        octoroute::models::health::HealthError::UnknownEndpoint(name) => {
            assert_eq!(name, "nonexistent-endpoint");
        }
        _ => panic!("Expected UnknownEndpoint, got {:?}", err),
    }
}

/// Test that AppError preserves HealthError without converting to string
///
/// **RED PHASE**: This test will fail because AppError::HealthTracking doesn't exist yet
#[tokio::test]
async fn test_app_error_preserves_health_error() {
    let config = Arc::new(create_test_config());
    let state = AppState::new(config).expect("should create AppState");

    // Simulate health tracking error by calling mark_success with unknown endpoint
    let health_result = state
        .selector()
        .health_checker()
        .mark_success("unknown-endpoint")
        .await;

    assert!(health_result.is_err());

    // Convert to AppError (this is what happens in handlers)
    let app_error: AppError = health_result.unwrap_err().into();

    // The AppError should preserve the HealthError type
    match app_error {
        AppError::HealthTracking(health_err) => {
            // Success! The error type is preserved
            match health_err {
                octoroute::models::health::HealthError::UnknownEndpoint(name) => {
                    assert_eq!(name, "unknown-endpoint");
                }
                _ => panic!("Expected UnknownEndpoint variant"),
            }
        }
        _ => panic!("Expected AppError::HealthTracking, got {:?}", app_error),
    }
}
