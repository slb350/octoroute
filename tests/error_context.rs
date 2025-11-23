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

/// Test that error source chain is preserved for ConfigFileRead
///
/// Verifies that io::Error context is preserved through the error chain
/// using the #[source] attribute, enabling proper error debugging.
#[test]
fn test_config_file_read_error_source_chain() {
    use std::error::Error;

    // Try to read a nonexistent file
    let result = Config::from_file("/nonexistent/path/to/config.toml");

    assert!(result.is_err(), "Should fail for nonexistent file");

    let app_error = result.unwrap_err();

    // Verify the error is ConfigFileRead
    match &app_error {
        AppError::ConfigFileRead { path, source, .. } => {
            assert!(path.contains("/nonexistent/path/to/config.toml"));
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        _ => panic!("Expected ConfigFileRead, got {:?}", app_error),
    }

    // Traverse the error source chain
    let source = app_error
        .source()
        .expect("AppError::ConfigFileRead should have a source");

    // The source should be an io::Error
    assert!(
        source.is::<std::io::Error>(),
        "Source should be std::io::Error, got: {:?}",
        source
    );

    // Downcast to io::Error to verify kind
    let io_error = source
        .downcast_ref::<std::io::Error>()
        .expect("Should downcast to io::Error");
    assert_eq!(io_error.kind(), std::io::ErrorKind::NotFound);
}

/// Test that error source chain is preserved for ConfigParseFailed
///
/// Verifies that toml::de::Error context (including line/column info)
/// is preserved through the error chain.
#[test]
fn test_config_parse_failed_error_source_chain() {
    use std::error::Error;

    // Create a temporary config file with invalid TOML
    let invalid_toml = r#"
[server]
host = "127.0.0.1
port = 3000
"#; // Missing closing quote

    let temp_dir = tempfile::tempdir().expect("should create temp dir");
    let config_path = temp_dir.path().join("invalid_config.toml");
    std::fs::write(&config_path, invalid_toml).expect("should write temp file");

    // Try to parse the invalid config
    let result = Config::from_file(&config_path);

    assert!(result.is_err(), "Should fail for invalid TOML");

    let app_error = result.unwrap_err();

    // Verify the error is ConfigParseFailed
    match &app_error {
        AppError::ConfigParseFailed { path, source } => {
            assert!(path.contains("invalid_config.toml"));
            // toml::de::Error should have line/column info
            let error_message = source.to_string();
            assert!(
                error_message.contains("line") || error_message.contains("column"),
                "TOML error should include line/column info: {}",
                error_message
            );
        }
        _ => panic!("Expected ConfigParseFailed, got {:?}", app_error),
    }

    // Traverse the error source chain
    let source = app_error
        .source()
        .expect("AppError::ConfigParseFailed should have a source");

    // The source should be a toml::de::Error
    assert!(
        source.is::<toml::de::Error>(),
        "Source should be toml::de::Error, got: {:?}",
        source
    );
}

/// Test that HybridRoutingFailed preserves the full error chain
///
/// Verifies that when hybrid routing fails, the original LLM routing error
/// is preserved through the error chain for debugging.
#[test]
fn test_hybrid_routing_failed_error_source_chain() {
    use octoroute::router::{Importance, TaskType};

    // Create a HybridRoutingFailed error with a nested LlmRouting error
    let llm_error = octoroute::router::llm_based::LlmRouterError::EmptyResponse {
        endpoint: "http://localhost:1234/v1".to_string(),
    };

    let hybrid_error = AppError::HybridRoutingFailed {
        prompt_preview: "test prompt".to_string(),
        task_type: TaskType::QuestionAnswer,
        importance: Importance::Normal,
        source: Box::new(AppError::LlmRouting(llm_error)),
    };

    // Verify the main error message
    let error_msg = hybrid_error.to_string();
    assert!(error_msg.contains("Hybrid routing failed"));
    assert!(error_msg.contains("QuestionAnswer"));
    assert!(error_msg.contains("Normal"));

    // Verify the source field directly (since it's a boxed AppError)
    // The HybridRoutingFailed error contains a Box<AppError> source
    match &hybrid_error {
        AppError::HybridRoutingFailed { source, .. } => match source.as_ref() {
            AppError::LlmRouting(llm_err) => match llm_err {
                octoroute::router::llm_based::LlmRouterError::EmptyResponse { endpoint } => {
                    assert_eq!(endpoint, "http://localhost:1234/v1");
                }
                _ => panic!("Expected EmptyResponse variant"),
            },
            _ => panic!("Expected AppError::LlmRouting in source field"),
        },
        _ => unreachable!("hybrid_error is already HybridRoutingFailed"),
    }
}
