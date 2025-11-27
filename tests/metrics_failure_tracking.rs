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

    // Should have a method to increment recording failures with operation label
    metrics.metrics_recording_failure("record_request");

    // Should have a method to get the count (sums across all labels)
    let count = metrics.metrics_recording_failures_count();
    assert_eq!(count, 1, "Counter should increment to 1");

    // Increment again
    metrics.metrics_recording_failure("record_request");
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

    // Simulate a metrics recording failure with operation label
    metrics.metrics_recording_failure("record_request");

    // Count should increment
    assert_eq!(
        metrics.metrics_recording_failures_count(),
        1,
        "Count should increment after failure"
    );

    // Multiple failures
    for _ in 0..5 {
        metrics.metrics_recording_failure("record_request");
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

    // Simulate some failures with operation label
    for _ in 0..3 {
        metrics.metrics_recording_failure("record_request");
    }

    // Gather metrics
    let output = metrics.gather().expect("should gather metrics");

    // Should include the counter
    assert!(
        output.contains("octoroute_metrics_recording_failures_total"),
        "Prometheus output should include metrics_recording_failures_total counter"
    );

    // Should show the count with operation label
    // Format: octoroute_metrics_recording_failures_total{operation="record_request"} 3
    assert!(
        output
            .contains("octoroute_metrics_recording_failures_total{operation=\"record_request\"} 3"),
        "Counter should show value of 3 with operation label, got:\n{}",
        output
    );
}

/// Test that metrics have bounded cardinality (no explosion from dynamic labels)
///
/// This test verifies that the type-safe enum approach prevents cardinality explosions.
/// Addresses PR #4 Medium Priority Issue #13.
///
/// **Background**: Prometheus metrics with high-cardinality labels (unbounded unique values)
/// cause memory exhaustion. For example, if error messages were used as labels:
/// - "Config file '/path/1' failed" -> series 1
/// - "Config file '/path/2' failed" -> series 2
/// - ... 1000 different paths -> 1000 series -> OOM
///
/// **This project's defense**: Use type-safe enums for all labels:
/// - Tier: Fast, Balanced, Deep (3 values)
/// - Strategy: Rule, Llm (2 values)
/// - Max cardinality: 3 * 2 = 6 unique series for request metrics
///
/// This test verifies that recording 1000 requests creates exactly 6 series, not 1000.
#[test]
fn test_metrics_cardinality_bounded_by_enums() {
    let metrics = Metrics::new().expect("should create metrics");

    // Record 1000 requests using all combinations of tier and strategy
    // This simulates heavy production load with diverse routing patterns
    for i in 0..1000 {
        let tier = match i % 3 {
            0 => Tier::Fast,
            1 => Tier::Balanced,
            _ => Tier::Deep,
        };
        let strategy = if i % 2 == 0 {
            Strategy::Rule
        } else {
            Strategy::Llm
        };

        metrics
            .record_request(tier, strategy)
            .expect("should record successfully");
    }

    // Gather metrics
    let output = metrics.gather().expect("should gather metrics");

    // Count unique series for octoroute_requests_total
    // Each line like: octoroute_requests_total{strategy="rule",tier="fast"} 167
    // Should have exactly 6 unique series (3 tiers * 2 strategies)
    let request_lines: Vec<&str> = output
        .lines()
        .filter(|line| line.starts_with("octoroute_requests_total{"))
        .collect();

    // Verify we have exactly 6 unique label combinations (bounded cardinality)
    assert_eq!(
        request_lines.len(),
        6,
        "Should have exactly 6 unique metric series (3 tiers * 2 strategies), not 1000. \
        Type-safe enums prevent cardinality explosion. Found {} series:\n{}",
        request_lines.len(),
        request_lines.join("\n")
    );

    // Verify all expected combinations exist
    let expected_combinations = [
        r#"strategy="rule",tier="fast""#,
        r#"strategy="rule",tier="balanced""#,
        r#"strategy="rule",tier="deep""#,
        r#"strategy="llm",tier="fast""#,
        r#"strategy="llm",tier="balanced""#,
        r#"strategy="llm",tier="deep""#,
    ];

    for expected in &expected_combinations {
        assert!(
            output.contains(expected),
            "Missing expected label combination: {}\nPrometheus output:\n{}",
            expected,
            output
        );
    }

    // Verify total count is 1000 (distributed across 6 series)
    let total_count: i32 = request_lines
        .iter()
        .filter_map(|line| {
            // Extract count from "...} 167"
            line.split_whitespace().last()?.parse::<i32>().ok()
        })
        .sum();

    assert_eq!(
        total_count, 1000,
        "Total requests across all series should be 1000"
    );
}
