//! Error message quality tests
//!
//! Tests that error messages are actionable and include:
//! - Tier names for routing errors
//! - Remediation suggestions
//! - Impact explanations (why errors are fatal)
//! - Adequate error context (500-char previews, not 100)
//!
//! Addresses PR #4 review issues: HIGH-1, HIGH-2, HIGH-3

use octoroute::router::llm_based::LlmRouterError;

/// Test that Timeout error includes tier name and remediation steps
#[test]
fn test_timeout_error_includes_tier_and_remediation() {
    let error = LlmRouterError::Timeout {
        endpoint: "http://localhost:1234/v1".to_string(),
        timeout_seconds: 10,
        attempt: 2,
        max_attempts: 3,
        router_tier: octoroute::router::TargetModel::Balanced,
    };

    let error_msg = error.to_string();

    // Should include timeout duration and attempt info
    assert!(
        error_msg.contains("10s") || error_msg.contains("10"),
        "Timeout error should include timeout duration, got: {}",
        error_msg
    );

    assert!(
        error_msg.contains("2") && error_msg.contains("3"),
        "Timeout error should include attempt count (2/3), got: {}",
        error_msg
    );

    // HIGH-1: Should include tier name
    // Note: This will FAIL initially - tier name not yet included
    assert!(
        error_msg.to_lowercase().contains("tier")
            || error_msg.contains("balanced")
            || error_msg.contains("fast")
            || error_msg.contains("deep"),
        "Timeout error should include tier name for context, got: {}",
        error_msg
    );

    // HIGH-1: Should include remediation suggestions
    // Note: This will FAIL initially - remediation not yet included
    let has_remediation = error_msg.to_lowercase().contains("check")
        || error_msg.to_lowercase().contains("increase")
        || error_msg.to_lowercase().contains("try")
        || error_msg.to_lowercase().contains("health")
        || error_msg.to_lowercase().contains("timeout")
        || error_msg.to_lowercase().contains("faster");

    assert!(
        has_remediation,
        "Timeout error should suggest remediation (check health, increase timeout, try faster tier), got: {}",
        error_msg
    );
}

/// Test that EmptyResponse error explains impact (why it's fatal)
#[test]
fn test_empty_response_explains_impact() {
    let error = LlmRouterError::EmptyResponse {
        endpoint: "http://localhost:1234/v1".to_string(),
    };

    let error_msg = error.to_string();

    // Should mention empty response
    assert!(
        error_msg.to_lowercase().contains("empty"),
        "Error should mention empty response, got: {}",
        error_msg
    );

    // HIGH-2: Should explain WHY empty response is fatal (expected format)
    // Note: This will FAIL initially - impact explanation not yet included
    let explains_impact = error_msg.to_lowercase().contains("expected")
        || error_msg.to_lowercase().contains("format")
        || error_msg.to_lowercase().contains("fast")
        || error_msg.to_lowercase().contains("balanced")
        || error_msg.to_lowercase().contains("deep")
        || error_msg.to_lowercase().contains("keyword");

    assert!(
        explains_impact,
        "Empty response error should explain expected format (FAST/BALANCED/DEEP keywords), got: {}",
        error_msg
    );

    // HIGH-2: Should list possible causes
    // Note: This will FAIL initially - causes not yet listed
    let lists_causes = error_msg.to_lowercase().contains("safety")
        || error_msg.to_lowercase().contains("filter")
        || error_msg.to_lowercase().contains("api")
        || error_msg.to_lowercase().contains("stream")
        || error_msg.to_lowercase().contains("misconfigur");

    assert!(
        lists_causes,
        "Empty response error should list possible causes (safety filter, API failure, streaming error, misconfiguration), got: {}",
        error_msg
    );
}

/// Test that UnparseableResponse error includes 500-char preview (not 100)
#[test]
fn test_unparseable_response_includes_500_char_preview() {
    // Create a response with 600 characters
    let long_response = "a".repeat(600);

    // Simulate the truncation that happens in llm_based.rs
    let response_preview = if long_response.len() > 500 {
        format!(
            "{}... [truncated]",
            &long_response.chars().take(500).collect::<String>()
        )
    } else {
        long_response.clone()
    };

    let error = LlmRouterError::UnparseableResponse {
        endpoint: "http://localhost:1234/v1".to_string(),
        response: response_preview,
        response_length: long_response.len(),
    };

    let error_msg = error.to_string();

    // Should mention unparseable
    assert!(
        error_msg.to_lowercase().contains("unparseable"),
        "Error should mention unparseable response, got first 100 chars: {}",
        &error_msg.chars().take(100).collect::<String>()
    );

    // HIGH-3: Should include 500-char preview (not 100)
    // Note: This will FAIL initially - current limit is 100 chars
    // Count how many 'a' characters are in the error message
    let a_count = error_msg.chars().filter(|&c| c == 'a').count();

    assert!(
        a_count >= 400,
        "Unparseable response error should include at least 400 chars of preview (target 500), got {} chars",
        a_count
    );

    assert!(
        a_count <= 550,
        "Unparseable response preview should not exceed 550 chars (target 500 + margin), got {} chars",
        a_count
    );

    // HIGH-3: Should include response length in error
    // Note: This will FAIL initially - length not yet included
    assert!(
        error_msg.contains("600") || error_msg.contains("length"),
        "Unparseable response error should include total response length (600 bytes), got: {}",
        &error_msg.chars().take(200).collect::<String>()
    );
}

/// Test that UnparseableResponse with short response shows full text (no truncation)
#[test]
fn test_unparseable_response_short_shows_full_text() {
    let short_response = "This is a short response with no keywords";

    let error = LlmRouterError::UnparseableResponse {
        endpoint: "http://localhost:1234/v1".to_string(),
        response: short_response.to_string(),
        response_length: short_response.len(),
    };

    let error_msg = error.to_string();

    // Should include full response text (no truncation for short responses)
    assert!(
        error_msg.contains(short_response),
        "Short unparseable response should be included in full without truncation, got: {}",
        error_msg
    );
}
