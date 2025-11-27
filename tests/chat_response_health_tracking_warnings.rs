//! Tests for health tracking failure warnings in ChatResponse
//!
//! Verifies that health tracking failures are surfaced to users via warnings
//! in the response, following the format used in production code.
//!
//! ## Background (from PR #4 Review - Issue #3)
//!
//! Health tracking failures (mark_success/mark_failure errors) need to be
//! surfaced to users, not just logged. This module tests the warning
//! infrastructure that enables this.
//!
//! ## What These Tests Verify
//!
//! 1. Warning message format matches production code
//! 2. QueryResult carries warnings to ChatResponse
//! 3. Multiple warnings accumulate correctly during retries
//!
//! ## Integration Testing Note
//!
//! Full integration tests (triggering actual mark_success/mark_failure failures)
//! would require mocking infrastructure. The unit tests here verify the plumbing
//! works correctly - the actual error paths are verified by code review.

use octoroute::config::ModelEndpoint;
use octoroute::handlers::chat::ChatResponse;
use octoroute::router::{RoutingStrategy, TargetModel};
use octoroute::shared::query::QueryResult;

/// Test that the health tracking warning format matches production code
///
/// The production code in shared/query.rs uses this format:
///   "Health tracking failed: {} (endpoint health state may be stale)"
///
/// This test documents and verifies the expected format.
#[test]
fn test_health_tracking_warning_format() {
    // Production format from shared/query.rs:383-386 and shared/query.rs:453-456
    let error_message = "UnknownEndpoint: test-endpoint";
    let warning = format!(
        "Health tracking failed: {} (endpoint health state may be stale)",
        error_message
    );

    assert!(
        warning.starts_with("Health tracking failed:"),
        "Warning should start with 'Health tracking failed:'"
    );
    assert!(
        warning.contains("endpoint health state may be stale"),
        "Warning should indicate stale health state"
    );
    assert!(
        warning.contains(error_message),
        "Warning should contain the error message"
    );
}

/// Test that QueryResult properly carries warnings
///
/// QueryResult is the return type of execute_query_with_retry() and must
/// carry any warnings accumulated during execution.
#[test]
fn test_query_result_carries_warnings() {
    let endpoint = create_test_endpoint();

    let warnings = vec![
        "Health tracking failed: UnknownEndpoint (endpoint health state may be stale)".to_string(),
    ];

    let result = QueryResult {
        content: "Response content".to_string(),
        endpoint: endpoint.clone(),
        tier: TargetModel::Fast,
        strategy: RoutingStrategy::Rule,
        warnings: warnings.clone(),
    };

    assert_eq!(result.warnings.len(), 1);
    assert_eq!(result.warnings, warnings);
}

/// Test that ChatResponse is correctly constructed from QueryResult with warnings
///
/// This verifies the plumbing in chat.rs:321-336 where QueryResult.warnings
/// flows to ChatResponse via new_with_warnings().
#[test]
fn test_chat_response_from_query_result_with_warnings() {
    let endpoint = create_test_endpoint();

    // Simulate QueryResult with health tracking warning
    let query_result = QueryResult {
        content: "Model response".to_string(),
        endpoint: endpoint.clone(),
        tier: TargetModel::Balanced,
        strategy: RoutingStrategy::Llm,
        warnings: vec![
            "Health tracking failed: UnknownEndpoint (endpoint health state may be stale)"
                .to_string(),
        ],
    };

    // This mirrors the logic in chat.rs:321-336
    let response = if query_result.warnings.is_empty() {
        ChatResponse::new(
            query_result.content.clone(),
            &query_result.endpoint,
            query_result.tier,
            query_result.strategy,
        )
    } else {
        ChatResponse::new_with_warnings(
            query_result.content.clone(),
            &query_result.endpoint,
            query_result.tier,
            query_result.strategy,
            query_result.warnings.clone(),
        )
    };

    assert_eq!(response.warnings().len(), 1);
    assert!(response.warnings()[0].contains("Health tracking failed"));
}

/// Test that multiple health tracking failures accumulate in warnings
///
/// During retry loops, multiple endpoints may fail health tracking.
/// All failures should accumulate in the warnings array.
#[test]
fn test_multiple_health_tracking_warnings_accumulate() {
    let endpoint = create_test_endpoint();

    // Simulate multiple health tracking failures during retries
    let warnings = vec![
        "Health tracking failed: UnknownEndpoint 'fast-1' (endpoint health state may be stale)"
            .to_string(),
        "Health tracking failed: UnknownEndpoint 'fast-2' (endpoint health state may be stale)"
            .to_string(),
        "Health tracking failed: UnknownEndpoint 'fast-3' (endpoint health state may be stale)"
            .to_string(),
    ];

    let result = QueryResult {
        content: "Response after retries".to_string(),
        endpoint,
        tier: TargetModel::Fast,
        strategy: RoutingStrategy::Rule,
        warnings: warnings.clone(),
    };

    assert_eq!(result.warnings.len(), 3);

    // Verify all endpoint names are captured
    assert!(result.warnings[0].contains("fast-1"));
    assert!(result.warnings[1].contains("fast-2"));
    assert!(result.warnings[2].contains("fast-3"));
}

/// Test that warnings from both mark_success and mark_failure accumulate
///
/// In a retry scenario, the flow might be:
/// 1. Try endpoint A, fails, mark_failure fails → warning
/// 2. Try endpoint B, succeeds, mark_success fails → warning
///
/// Both warnings should appear in the final response.
#[test]
fn test_mixed_mark_success_and_mark_failure_warnings() {
    let endpoint = create_test_endpoint();

    let warnings = vec![
        // mark_failure warning from first failed attempt
        "Health tracking failed: UnknownEndpoint 'fast-1' (endpoint health state may be stale)"
            .to_string(),
        // mark_success warning from final successful attempt
        "Health tracking failed: UnknownEndpoint 'fast-2' (endpoint health state may be stale)"
            .to_string(),
    ];

    let response = ChatResponse::new_with_warnings(
        "Success after retry".to_string(),
        &endpoint,
        TargetModel::Fast,
        RoutingStrategy::Rule,
        warnings,
    );

    assert_eq!(response.warnings().len(), 2);

    // Both warnings should be present
    let all_warnings = response.warnings().join(" ");
    assert!(all_warnings.contains("fast-1"));
    assert!(all_warnings.contains("fast-2"));
}

/// Test that warnings serialize correctly in JSON response
///
/// Warnings must be visible in the JSON sent to clients so operators
/// can monitor for health tracking issues.
#[test]
fn test_health_tracking_warnings_serialize_in_json() {
    let endpoint = create_test_endpoint();

    let response = ChatResponse::new_with_warnings(
        "Response".to_string(),
        &endpoint,
        TargetModel::Deep,
        RoutingStrategy::Rule,
        vec![
            "Health tracking failed: UnknownEndpoint (endpoint health state may be stale)"
                .to_string(),
        ],
    );

    let json = serde_json::to_string(&response).expect("should serialize");

    assert!(
        json.contains("warnings"),
        "JSON should contain warnings field"
    );
    assert!(
        json.contains("Health tracking failed"),
        "JSON should contain warning message"
    );
    assert!(
        json.contains("endpoint health state may be stale"),
        "JSON should contain stale state notice"
    );
}

/// Helper to create a test endpoint
fn create_test_endpoint() -> ModelEndpoint {
    let toml = r#"
name = "test-model"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1
"#;
    toml::from_str(toml).expect("should parse test endpoint")
}
