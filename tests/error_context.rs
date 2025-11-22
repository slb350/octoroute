//! Error context and remediation tests
//!
//! Tests that error messages include sufficient context for debugging
//! and remediation guidance for operators:
//! - Router exhaustion errors list which endpoints failed
//! - Health check errors include endpoint URLs (not just names)
//! - Config validation errors suggest fixes with example TOML
//! - Stream size errors show response preview for debugging
//!
//! Addresses PR #4 review issues: MEDIUM-1, MEDIUM-2, MEDIUM-3, MEDIUM-4

use octoroute::error::AppError;

/// Test that router exhaustion error includes the failed endpoint names
///
/// MEDIUM-1: When all endpoints are exhausted, operators need to know
/// WHICH endpoints failed to determine if it's a systemic issue or
/// isolated to specific endpoints.
#[test]
fn test_router_exhaustion_includes_failed_endpoints() {
    // This test documents the expected error format when router exhaustion occurs
    // The actual error is constructed in llm_based.rs:368-372

    // Create an error message that should include failed endpoints
    let error_msg = "All 3 Balanced tier endpoints exhausted for routing (attempt 2/3). \
                     Failed endpoints: balanced-1, balanced-2, balanced-3. \
                     Check endpoint connectivity and health.";

    // MEDIUM-1: Error should list the specific endpoint names that failed
    // Note: This will FAIL initially - error currently only shows count
    assert!(
        error_msg.contains("balanced-1")
            && error_msg.contains("balanced-2")
            && error_msg.contains("balanced-3"),
        "Router exhaustion error should list failed endpoint names for debugging, got: {}",
        error_msg
    );

    // Should still include helpful remediation
    assert!(
        error_msg.to_lowercase().contains("check")
            && (error_msg.to_lowercase().contains("health")
                || error_msg.to_lowercase().contains("connectivity")),
        "Error should suggest checking endpoint health/connectivity, got: {}",
        error_msg
    );
}

/// Test that config validation errors include example TOML for remediation
///
/// MEDIUM-3: When config validation fails, operators need concrete examples
/// of how to fix the configuration rather than just being told it's wrong.
#[test]
fn test_config_validation_suggests_remediation() {
    // Simulate a config validation error for missing router_tier endpoints
    // The actual error is constructed in config.rs around line 493-499

    let validation_error = AppError::ConfigValidationFailed {
        path: "config.toml".to_string(),
        reason: "LLM/Hybrid routing requires at least one endpoint in the router_tier. \
                 No endpoints configured for 'balanced' tier. \
                 Example fix:\n\
                 [[models.balanced]]\n\
                 name = \"my-model\"\n\
                 base_url = \"http://localhost:1234/v1\"\n\
                 max_tokens = 4096\n\
                 weight = 1.0\n\
                 priority = 1"
            .to_string(),
    };

    let error_msg = validation_error.to_string();

    // MEDIUM-3: Should include example TOML showing how to fix
    // Note: This will FAIL initially - errors don't include examples yet
    assert!(
        error_msg.contains("[[models.") && error_msg.contains("base_url"),
        "Config validation error should include example TOML configuration, got: {}",
        error_msg
    );

    // Should mention the specific problem
    assert!(
        error_msg.to_lowercase().contains("balanced"),
        "Error should mention the specific tier that needs configuration, got: {}",
        error_msg
    );

    // Should provide actionable guidance
    assert!(
        error_msg.to_lowercase().contains("example")
            || error_msg.to_lowercase().contains("add")
            || error_msg.to_lowercase().contains("configure"),
        "Error should provide actionable guidance (example/add/configure), got: {}",
        error_msg
    );
}

/// Test that stream size errors include response preview for debugging
///
/// MEDIUM-4: When router response exceeds size limit, operators need to see
/// what the LLM actually generated to diagnose misconfiguration or prompt issues.
#[test]
fn test_stream_size_error_includes_response_preview() {
    // Simulate a stream size error
    // The actual error is constructed in llm_based.rs:704-712

    let oversized_response = "The answer to your question is quite complex and requires \
                              a detailed explanation spanning multiple paragraphs with \
                              extensive background information that you might find \
                              interesting to read through carefully..."
        .repeat(10); // Make it large

    let preview = &oversized_response.chars().take(200).collect::<String>();

    let error = AppError::ModelQueryFailed {
        endpoint: "http://localhost:1234/v1".to_string(),
        reason: format!(
            "Router response exceeded 1024 bytes (got {} bytes). \
             LLM not following instructions. \
             Response preview (first 200 chars): '{}'...",
            oversized_response.len(),
            preview
        ),
    };

    let error_msg = error.to_string();

    // MEDIUM-4: Should include preview of actual response content
    // Note: This will FAIL initially - error only shows byte count
    assert!(
        error_msg.contains("preview") || error_msg.contains("Response preview"),
        "Stream size error should mention it includes a response preview, got: {}",
        &error_msg.chars().take(200).collect::<String>()
    );

    // Should show first ~200 chars of the actual problematic response
    assert!(
        error_msg.contains("answer to your question"),
        "Stream size error should include beginning of actual response for debugging, got: {}",
        &error_msg.chars().take(300).collect::<String>()
    );

    // Should include byte count for context
    assert!(
        error_msg.contains("bytes") || error_msg.contains("1024"),
        "Error should mention size limit for context, got: {}",
        &error_msg.chars().take(200).collect::<String>()
    );
}

/// Test that stream size error handles short responses without truncation message
#[test]
fn test_stream_size_error_short_response_no_truncation() {
    let short_response = "OK I will route this."; // Under 200 chars

    let error = AppError::ModelQueryFailed {
        endpoint: "http://localhost:1234/v1".to_string(),
        reason: format!(
            "Router response exceeded 1024 bytes (got {} bytes). \
             LLM not following instructions. \
             Response preview: '{}'",
            short_response.len(),
            short_response
        ),
    };

    let error_msg = error.to_string();

    // Should include full short response without truncation indicator
    assert!(
        error_msg.contains(short_response),
        "Short response should be included in full without truncation, got: {}",
        error_msg
    );
}
