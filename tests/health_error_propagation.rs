//! Integration tests for health tracking error propagation
//!
//! Verifies that health tracking errors (mark_success/mark_failure failures)
//! propagate correctly through the request handling chain and don't get
//! silently swallowed.

use octoroute::config::Config;
use octoroute::models::{HealthChecker, ModelSelector};
use std::sync::Arc;

fn create_test_config() -> Config {
    let toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1235/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:1236/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "rule"
router_model = "balanced"
"#;
    toml::from_str(toml).expect("should parse TOML")
}

#[tokio::test]
async fn test_mark_success_with_unknown_endpoint_returns_error() {
    // This test verifies that calling mark_success with an unknown endpoint name
    // returns an error rather than silently failing or panicking.
    //
    // Scenario: A race condition where an endpoint is selected but then removed
    // from config before health status can be updated. While unlikely in the
    // current implementation (Config is immutable), this test documents expected
    // behavior and catches bugs if mutable config is introduced later.

    let config = Arc::new(create_test_config());
    let checker = HealthChecker::new(config);

    // Attempt to mark success for non-existent endpoint
    let result = checker.mark_success("non-existent-endpoint").await;

    assert!(
        result.is_err(),
        "mark_success with unknown endpoint should return error"
    );

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Unknown endpoint"),
        "error should mention unknown endpoint, got: {}",
        err
    );
}

#[tokio::test]
async fn test_mark_failure_with_unknown_endpoint_returns_error() {
    // Similar to mark_success test, but for mark_failure

    let config = Arc::new(create_test_config());
    let checker = HealthChecker::new(config);

    // Attempt to mark failure for non-existent endpoint
    let result = checker.mark_failure("non-existent-endpoint").await;

    assert!(
        result.is_err(),
        "mark_failure with unknown endpoint should return error"
    );

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("Unknown endpoint"),
        "error should mention unknown endpoint, got: {}",
        err
    );
}

#[tokio::test]
async fn test_health_error_has_debug_and_display_impl() {
    // Verify that HealthError can be logged and displayed properly
    // This is important for error propagation and debugging

    let config = Arc::new(create_test_config());
    let checker = HealthChecker::new(config);

    let result = checker.mark_success("non-existent").await;
    let err = result.unwrap_err();

    // Test Debug impl
    let debug_str = format!("{:?}", err);
    assert!(
        !debug_str.is_empty(),
        "HealthError should have Debug implementation"
    );

    // Test Display impl (via to_string)
    let display_str = err.to_string();
    assert!(
        !display_str.is_empty(),
        "HealthError should have Display implementation"
    );
    assert!(
        display_str.contains("Unknown endpoint"),
        "Display should mention unknown endpoint"
    );
}

#[tokio::test]
async fn test_selector_with_health_checker_integration() {
    // Integration test: Verify that ModelSelector + HealthChecker work together
    // and that health errors propagate correctly through the selection process

    let config = Arc::new(create_test_config());
    let selector = ModelSelector::new(config.clone());

    // Get initial health status - all endpoints should be healthy initially
    let initial_health = selector.health_checker().get_all_statuses().await;
    assert_eq!(
        initial_health.len(),
        3,
        "should have 3 endpoints (fast-1, balanced-1, deep-1)"
    );

    // Verify we can call mark_success/mark_failure with valid endpoints
    let result = selector.health_checker().mark_success("fast-1").await;
    assert!(
        result.is_ok(),
        "mark_success with valid endpoint should succeed"
    );

    let result = selector.health_checker().mark_failure("balanced-1").await;
    assert!(
        result.is_ok(),
        "mark_failure with valid endpoint should succeed"
    );

    // Verify invalid endpoint still returns error
    let result = selector.health_checker().mark_success("invalid").await;
    assert!(
        result.is_err(),
        "mark_success with invalid endpoint should return error"
    );
}
