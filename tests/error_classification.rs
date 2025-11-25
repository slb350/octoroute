//! Tests for type-safe error classification
//!
//! Verifies that error retryability is determined by typed error variants
//! instead of fragile string matching, addressing CRITICAL-4 from PR #4 review.
//!
//! ## Background
//!
//! Current implementation uses substring matching like `reason.contains("unparseable")`
//! which is fragile and error-prone. This test suite ensures we use exhaustive
//! pattern matching on typed error variants instead.

/// Test that Timeout errors are classified as retryable
#[test]
fn test_timeout_error_is_retryable() {
    // RED: ModelQueryError::Timeout should be retryable
    //
    // Timeout errors are transient - endpoint may be overloaded or unreachable.
    // Retrying with a different endpoint may succeed.

    use octoroute::error::ModelQueryError;

    let error = ModelQueryError::Timeout {
        endpoint: "http://localhost:1234/v1".to_string(),
        timeout_seconds: 30,
        attempt: 1,
        max_attempts: 3,
    };

    assert!(
        error.is_retryable(),
        "Timeout errors should be retryable (transient network/endpoint issue)"
    );
}

/// Test that UnparseableResponse errors are NOT retryable
#[test]
fn test_unparseable_response_is_not_retryable() {
    // RED: ModelQueryError::UnparseableResponse should NOT be retryable
    //
    // Unparseable responses indicate systemic issues (LLM malfunction, safety filter, etc.)
    // that won't be fixed by retrying with a different endpoint.

    use octoroute::error::ModelQueryError;

    let error = ModelQueryError::UnparseableResponse {
        endpoint: "http://localhost:1234/v1".to_string(),
        response: "INVALID JSON{}".to_string(),
    };

    assert!(
        !error.is_retryable(),
        "UnparseableResponse errors should NOT be retryable (systemic issue)"
    );
}

/// Test that EmptyResponse errors are NOT retryable
#[test]
fn test_empty_response_is_not_retryable() {
    // RED: ModelQueryError::EmptyResponse should NOT be retryable

    use octoroute::error::ModelQueryError;

    let error = ModelQueryError::EmptyResponse {
        endpoint: "http://localhost:1234/v1".to_string(),
    };

    assert!(
        !error.is_retryable(),
        "EmptyResponse errors should NOT be retryable (LLM malfunction)"
    );
}

/// Test that StreamError errors are retryable
#[test]
fn test_stream_error_is_retryable() {
    // RED: ModelQueryError::StreamError should be retryable
    //
    // Stream errors are transient network interruptions that may succeed
    // with a different endpoint.

    use octoroute::error::ModelQueryError;

    let error = ModelQueryError::StreamError {
        endpoint: "http://localhost:1234/v1".to_string(),
        bytes_received: 1024,
        error_message: "connection reset by peer".to_string(),
    };

    assert!(
        error.is_retryable(),
        "StreamError errors should be retryable (transient network issue)"
    );
}

/// Test that AgentOptionsConfigError is NOT retryable
#[test]
fn test_agent_options_config_error_is_not_retryable() {
    // RED: ModelQueryError::AgentOptionsConfigError should NOT be retryable

    use octoroute::error::ModelQueryError;

    let error = ModelQueryError::AgentOptionsConfigError {
        endpoint: "http://localhost:1234/v1".to_string(),
        details: "invalid model name".to_string(),
    };

    assert!(
        !error.is_retryable(),
        "AgentOptionsConfigError should NOT be retryable (configuration problem)"
    );
}

/// Test that all error variants have explicit classification
#[test]
fn test_exhaustive_error_classification() {
    // RED: is_retryable() should use exhaustive match to ensure all variants are classified
    //
    // This test ensures we don't accidentally add a new variant without
    // explicitly classifying its retryability.

    use octoroute::error::ModelQueryError;

    // Retryable errors (transient network/endpoint issues)
    let retryable_errors = vec![
        ModelQueryError::Timeout {
            endpoint: "test".to_string(),
            timeout_seconds: 30,
            attempt: 1,
            max_attempts: 3,
        },
        ModelQueryError::StreamError {
            endpoint: "test".to_string(),
            bytes_received: 0,
            error_message: "test".to_string(),
        },
    ];

    for error in retryable_errors {
        assert!(error.is_retryable(), "Expected {:?} to be retryable", error);
    }

    // Non-retryable errors (systemic issues)
    let non_retryable_errors = vec![
        ModelQueryError::EmptyResponse {
            endpoint: "test".to_string(),
        },
        ModelQueryError::UnparseableResponse {
            endpoint: "test".to_string(),
            response: "test".to_string(),
        },
        ModelQueryError::AgentOptionsConfigError {
            endpoint: "test".to_string(),
            details: "test".to_string(),
        },
    ];

    for error in non_retryable_errors {
        assert!(
            !error.is_retryable(),
            "Expected {:?} to be NOT retryable",
            error
        );
    }
}

/// Test that AppError correctly delegates to ModelQueryError.is_retryable()
#[test]
fn test_app_error_delegates_to_model_query_error() {
    // RED: AppError::ModelQuery should delegate to ModelQueryError::is_retryable()

    use octoroute::error::{AppError, ModelQueryError};

    // Wrap retryable error in AppError
    let retryable = AppError::ModelQuery(ModelQueryError::Timeout {
        endpoint: "test".to_string(),
        timeout_seconds: 30,
        attempt: 1,
        max_attempts: 3,
    });

    // Wrap non-retryable error in AppError
    let non_retryable = AppError::ModelQuery(ModelQueryError::EmptyResponse {
        endpoint: "test".to_string(),
    });

    // Test using the same is_retryable_error function that LlmBasedRouter uses
    // (This will be a helper function we add to make the logic testable)
    assert!(
        is_error_retryable(&retryable),
        "Timeout should be retryable when wrapped in AppError"
    );
    assert!(
        !is_error_retryable(&non_retryable),
        "EmptyResponse should NOT be retryable when wrapped in AppError"
    );
}

// Helper function to test error classification (will match LlmBasedRouter's logic)
fn is_error_retryable(error: &octoroute::error::AppError) -> bool {
    use octoroute::error::AppError;

    match error {
        AppError::ModelQuery(e) => e.is_retryable(),
        AppError::LlmRouting(e) => e.is_retryable(),
        AppError::Config(_)
        | AppError::ConfigFileRead { .. }
        | AppError::ConfigParseFailed { .. }
        | AppError::ConfigValidationFailed { .. } => false,
        _ => true, // Conservative: assume retryable for unknown errors
    }
}
