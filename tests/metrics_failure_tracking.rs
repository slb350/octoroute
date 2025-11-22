//! Tests for metrics recording failure tracking
//!
//! Verifies that metrics recording failures are tracked and surfaced
//! to operators via counters and /health endpoint.
//!
//! Addresses PR #4 review: Critical Issue #1 - Metrics failures silently suppressed

use octoroute::metrics::{Metrics, Strategy, Tier};

/// Test that Metrics has a counter for recording failures
///
/// **RED PHASE**: This test will fail because metrics_recording_failures() doesn't exist yet
#[test]
fn test_metrics_has_recording_failures_counter() {
    let metrics = Metrics::new().expect("should create metrics");

    // Should have a method to increment recording failures
    metrics.metrics_recording_failure();

    // Should have a method to get the count
    let count = metrics.metrics_recording_failures_count();
    assert_eq!(count, 1, "Counter should increment to 1");

    // Increment again
    metrics.metrics_recording_failure();
    let count = metrics.metrics_recording_failures_count();
    assert_eq!(count, 2, "Counter should increment to 2");
}

/// Test that metrics recording failures can be tracked
///
/// **RED PHASE**: This test will fail because metrics_recording_failure() doesn't exist yet
#[test]
fn test_metrics_recording_failure_tracking() {
    let metrics = Metrics::new().expect("should create metrics");

    // Initially zero
    assert_eq!(
        metrics.metrics_recording_failures_count(),
        0,
        "Initial count should be 0"
    );

    // Simulate a metrics recording failure
    metrics.metrics_recording_failure();

    // Count should increment
    assert_eq!(
        metrics.metrics_recording_failures_count(),
        1,
        "Count should increment after failure"
    );

    // Multiple failures
    for _ in 0..5 {
        metrics.metrics_recording_failure();
    }

    assert_eq!(
        metrics.metrics_recording_failures_count(),
        6,
        "Count should be 6 after 6 total failures"
    );
}

/// Test that normal metrics recording doesn't affect failure counter
///
/// **RED PHASE**: This test will fail because metrics_recording_failure() doesn't exist yet
#[test]
fn test_successful_metrics_dont_increment_failure_counter() {
    let metrics = Metrics::new().expect("should create metrics");

    // Record successful metrics
    metrics
        .record_request(Tier::Fast, Strategy::Rule)
        .expect("should record successfully");
    metrics
        .record_routing_duration(Strategy::Rule, 100.0)
        .expect("should record successfully");
    metrics
        .record_model_invocation(Tier::Fast)
        .expect("should record successfully");

    // Failure counter should still be 0
    assert_eq!(
        metrics.metrics_recording_failures_count(),
        0,
        "Successful metrics recording should not increment failure counter"
    );
}

/// Test that metrics recording failures are surfaced in gathered metrics
///
/// **RED PHASE**: This test will fail because counter doesn't exist yet
#[test]
fn test_metrics_recording_failures_in_prometheus_output() {
    let metrics = Metrics::new().expect("should create metrics");

    // Simulate some failures
    for _ in 0..3 {
        metrics.metrics_recording_failure();
    }

    // Gather metrics
    let output = metrics.gather().expect("should gather metrics");

    // Should include the counter
    assert!(
        output.contains("octoroute_metrics_recording_failures_total"),
        "Prometheus output should include metrics_recording_failures_total counter"
    );

    // Should show the count
    assert!(
        output.contains("octoroute_metrics_recording_failures_total 3"),
        "Counter should show value of 3, got:\n{}",
        output
    );
}
