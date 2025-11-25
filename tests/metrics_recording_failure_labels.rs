//! Tests for Prometheus metric labels on metrics recording failures
//!
//! Verifies that metrics_recording_failures_total metric includes labels for:
//! - operation: Which metric operation failed (record_request, record_routing_duration, record_model_invocation)
//!
//! ## Rationale (from PR #4 Review - Issue #4)
//!
//! Without labels, Prometheus alerts like "metrics_recording_failures_total is increasing"
//! provide no context about which metric operation is failing. Operators must dig through
//! logs to identify the specific metric operation that's experiencing issues.
//!
//! With labels, alerts can show:
//! - "Metrics recording failures for operation record_request"
//! - Enables operation-specific alerting and debugging

use octoroute::metrics::Metrics;

/// RED PHASE: Test that metrics_recording_failure() accepts operation label
///
/// This test will FAIL because current implementation:
/// - metrics_recording_failures is IntCounter (no labels)
/// - metrics_recording_failure() method takes no parameters
///
/// Expected after implementation:
/// - metrics_recording_failures is IntCounterVec with labels ["operation"]
/// - metrics_recording_failure(operation) method accepts parameter
#[test]
fn test_metrics_recording_failure_metric_has_labels() {
    let metrics = Metrics::new().expect("should create Metrics");

    // Record metrics recording failures for different operations
    metrics.metrics_recording_failure("record_request");
    metrics.metrics_recording_failure("record_request"); // Same operation twice
    metrics.metrics_recording_failure("record_routing_duration");
    metrics.metrics_recording_failure("record_model_invocation");

    // Gather metrics for verification
    let metric_families = metrics.registry.gather();

    // Find the metrics_recording_failures metric
    let metrics_recording_metric = metric_families
        .iter()
        .find(|mf| mf.name() == "octoroute_metrics_recording_failures_total")
        .expect("should find metrics_recording_failures_total metric");

    // Verify it's a counter
    assert_eq!(
        metrics_recording_metric.get_field_type(),
        prometheus::proto::MetricType::COUNTER,
        "metrics_recording_failures should be a COUNTER"
    );

    // Get all metric samples (one per unique label combination)
    let metrics = metrics_recording_metric.get_metric();

    // Should have 3 unique label combinations:
    // 1. operation=record_request (count: 2)
    // 2. operation=record_routing_duration (count: 1)
    // 3. operation=record_model_invocation (count: 1)
    assert_eq!(metrics.len(), 3, "should have 3 unique label combinations");

    // Verify each label combination and its count
    for metric in metrics {
        let labels = metric.get_label();

        // Extract label value
        let operation = labels
            .iter()
            .find(|l| l.name() == "operation")
            .expect("should have operation label")
            .value();

        let count = metric.get_counter().value.unwrap_or(0.0) as u64;

        // Verify expected combinations
        match operation {
            "record_request" => {
                assert_eq!(count, 2, "record_request should have count 2");
            }
            "record_routing_duration" => {
                assert_eq!(count, 1, "record_routing_duration should have count 1");
            }
            "record_model_invocation" => {
                assert_eq!(count, 1, "record_model_invocation should have count 1");
            }
            op => {
                panic!("Unexpected operation label: {}", op);
            }
        }
    }
}

/// Test that operation labels match actual metric operation names
///
/// This ensures consistency between the operation types used in metrics
/// and the actual metric operation method names.
#[test]
fn test_metrics_recording_operation_types_are_consistent() {
    let metrics = Metrics::new().expect("should create Metrics");

    // These should match the actual method names from Metrics
    // See src/metrics.rs for method definitions
    let valid_operations = vec![
        "record_request",
        "record_routing_duration",
        "record_model_invocation",
    ];

    // Record one of each type to verify they're all valid
    for operation in &valid_operations {
        metrics.metrics_recording_failure(operation);
    }

    // If we get here without panicking, all operations are valid
    let metric_families = metrics.registry.gather();
    let metrics_recording_metric = metric_families
        .iter()
        .find(|mf| mf.name() == "octoroute_metrics_recording_failures_total")
        .expect("should find metric");

    // Should have one metric per operation
    assert_eq!(
        metrics_recording_metric.get_metric().len(),
        valid_operations.len(),
        "should have metrics for all operations"
    );
}

/// Test that total count across all labels matches expectations
///
/// Verifies that the labeled metric still provides a total count
/// when queried without label filters.
#[test]
fn test_metrics_recording_failure_total_count() {
    let metrics = Metrics::new().expect("should create Metrics");

    // Record 10 failures across different operations
    metrics.metrics_recording_failure("record_request");
    metrics.metrics_recording_failure("record_request");
    metrics.metrics_recording_failure("record_request");
    metrics.metrics_recording_failure("record_routing_duration");
    metrics.metrics_recording_failure("record_routing_duration");
    metrics.metrics_recording_failure("record_routing_duration");
    metrics.metrics_recording_failure("record_model_invocation");
    metrics.metrics_recording_failure("record_model_invocation");
    metrics.metrics_recording_failure("record_model_invocation");
    metrics.metrics_recording_failure("record_model_invocation");

    // Total count should be 10 (sum across all label combinations)
    let metric_families = metrics.registry.gather();
    let metrics_recording_metric = metric_families
        .iter()
        .find(|mf| mf.name() == "octoroute_metrics_recording_failures_total")
        .expect("should find metric");

    let total_count: u64 = metrics_recording_metric
        .get_metric()
        .iter()
        .map(|m| m.get_counter().value.unwrap_or(0.0) as u64)
        .sum();

    assert_eq!(total_count, 10, "total count should be 10");
}
