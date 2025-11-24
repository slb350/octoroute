//! Tests for Prometheus metric labels on health tracking failures
//!
//! Verifies that health_tracking_failures_total metric includes labels for:
//! - endpoint: Which endpoint experienced the tracking failure
//! - error_type: What type of error occurred (unknown_endpoint, http_client_failed, invalid_url)
//!
//! ## Rationale (from PR #4 Review - Issue #4)
//!
//! Without labels, Prometheus alerts like "health_tracking_failures_total is increasing"
//! provide no context about which endpoint or what type of failure occurred.
//! Operators must dig through logs to identify the failing endpoint.
//!
//! With labels, alerts can show:
//! - "Health tracking failures on endpoint fast-1 (error: unknown_endpoint)"
//! - Enables endpoint-specific alerting and debugging

use octoroute::metrics::Metrics;
use prometheus::Registry;

/// RED PHASE: Test that health_tracking_failure() accepts endpoint and error_type labels
///
/// This test will FAIL because current implementation:
/// - health_tracking_failures is IntCounter (no labels)
/// - health_tracking_failure() method takes no parameters
///
/// Expected after implementation:
/// - health_tracking_failures is IntCounterVec with labels ["endpoint", "error_type"]
/// - health_tracking_failure(endpoint, error_type) method accepts parameters
#[test]
fn test_health_tracking_failure_metric_has_labels() {
    let metrics = Metrics::new().expect("should create Metrics");

    // Record health tracking failures for different endpoints and error types
    metrics.health_tracking_failure("fast-1", "unknown_endpoint");
    metrics.health_tracking_failure("fast-1", "unknown_endpoint"); // Same endpoint, same error
    metrics.health_tracking_failure("balanced-1", "http_client_failed");
    metrics.health_tracking_failure("deep-1", "invalid_url");

    // Gather metrics for verification
    let metric_families = metrics.registry.gather();

    // Find the health_tracking_failures metric
    let health_tracking_metric = metric_families
        .iter()
        .find(|mf| mf.get_name() == "octoroute_health_tracking_failures_total")
        .expect("should find health_tracking_failures_total metric");

    // Verify it's a counter
    assert_eq!(
        health_tracking_metric.get_field_type(),
        prometheus::proto::MetricType::COUNTER,
        "health_tracking_failures should be a COUNTER"
    );

    // Get all metric samples (one per unique label combination)
    let metrics = health_tracking_metric.get_metric();

    // Should have 3 unique label combinations:
    // 1. endpoint=fast-1, error_type=unknown_endpoint (count: 2)
    // 2. endpoint=balanced-1, error_type=http_client_failed (count: 1)
    // 3. endpoint=deep-1, error_type=invalid_url (count: 1)
    assert_eq!(metrics.len(), 3, "should have 3 unique label combinations");

    // Verify each label combination and its count
    for metric in metrics {
        let labels = metric.get_label();

        // Extract label values
        let endpoint = labels
            .iter()
            .find(|l| l.get_name() == "endpoint")
            .expect("should have endpoint label")
            .get_value();

        let error_type = labels
            .iter()
            .find(|l| l.get_name() == "error_type")
            .expect("should have error_type label")
            .get_value();

        let count = metric.get_counter().value.unwrap_or(0.0) as u64;

        // Verify expected combinations
        match (endpoint, error_type) {
            ("fast-1", "unknown_endpoint") => {
                assert_eq!(count, 2, "fast-1 unknown_endpoint should have count 2");
            }
            ("balanced-1", "http_client_failed") => {
                assert_eq!(
                    count, 1,
                    "balanced-1 http_client_failed should have count 1"
                );
            }
            ("deep-1", "invalid_url") => {
                assert_eq!(count, 1, "deep-1 invalid_url should have count 1");
            }
            (ep, et) => {
                panic!(
                    "Unexpected label combination: endpoint={}, error_type={}",
                    ep, et
                );
            }
        }
    }
}

/// Test that error_type labels match actual HealthError variants
///
/// This ensures consistency between the error types used in metrics
/// and the actual HealthError enum variants.
#[test]
fn test_health_tracking_error_types_are_consistent() {
    let metrics = Metrics::new().expect("should create Metrics");

    // These should match the HealthError enum variants (lowercased snake_case)
    // See src/models/health.rs for HealthError definition
    let valid_error_types = vec!["unknown_endpoint", "http_client_failed", "invalid_url"];

    // Record one of each type to verify they're all valid
    for error_type in &valid_error_types {
        metrics.health_tracking_failure("test-endpoint", error_type);
    }

    // If we get here without panicking, all error types are valid
    let metric_families = metrics.registry.gather();
    let health_tracking_metric = metric_families
        .iter()
        .find(|mf| mf.get_name() == "octoroute_health_tracking_failures_total")
        .expect("should find metric");

    // Should have one metric per error type
    assert_eq!(
        health_tracking_metric.get_metric().len(),
        valid_error_types.len(),
        "should have metrics for all error types"
    );
}

/// Test that total count across all labels matches expectations
///
/// Verifies that the labeled metric still provides a total count
/// when queried without label filters.
#[test]
fn test_health_tracking_failure_total_count() {
    let metrics = Metrics::new().expect("should create Metrics");

    // Record 10 failures across different endpoints and error types
    metrics.health_tracking_failure("fast-1", "unknown_endpoint");
    metrics.health_tracking_failure("fast-1", "unknown_endpoint");
    metrics.health_tracking_failure("fast-2", "unknown_endpoint");
    metrics.health_tracking_failure("balanced-1", "http_client_failed");
    metrics.health_tracking_failure("balanced-1", "http_client_failed");
    metrics.health_tracking_failure("balanced-1", "http_client_failed");
    metrics.health_tracking_failure("deep-1", "invalid_url");
    metrics.health_tracking_failure("deep-1", "invalid_url");
    metrics.health_tracking_failure("deep-1", "invalid_url");
    metrics.health_tracking_failure("deep-1", "invalid_url");

    // Total count should be 10 (sum across all label combinations)
    let metric_families = metrics.registry.gather();
    let health_tracking_metric = metric_families
        .iter()
        .find(|mf| mf.get_name() == "octoroute_health_tracking_failures_total")
        .expect("should find metric");

    let total_count: u64 = health_tracking_metric
        .get_metric()
        .iter()
        .map(|m| m.get_counter().value.unwrap_or(0.0) as u64)
        .sum();

    assert_eq!(total_count, 10, "total count should be 10");
}
