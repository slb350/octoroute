//! LlmRouterError Tests

use super::*;
use crate::error::AppError;

#[test]
fn test_llmroutererror_systemic_errors_not_retryable() {
    // Verify all systemic error variants return false for is_retryable()

    let empty = LlmRouterError::EmptyResponse {
        endpoint: "http://test:1234/v1".to_string(),
    };
    assert!(
        !empty.is_retryable(),
        "EmptyResponse should not be retryable (systemic)"
    );

    let unparseable = LlmRouterError::UnparseableResponse {
        endpoint: "http://test:1234/v1".to_string(),
        response: "BAD RESPONSE".to_string(),
        response_length: 12,
    };
    assert!(
        !unparseable.is_retryable(),
        "UnparseableResponse should not be retryable (systemic)"
    );

    let refusal = LlmRouterError::Refusal {
        endpoint: "http://test:1234/v1".to_string(),
        message: "Cannot process this request".to_string(),
    };
    assert!(
        !refusal.is_retryable(),
        "Refusal should not be retryable (systemic)"
    );

    let size_exceeded = LlmRouterError::SizeExceeded {
        endpoint: "http://test:1234/v1".to_string(),
        size: 2048,
        max_size: 1024,
    };
    assert!(
        !size_exceeded.is_retryable(),
        "SizeExceeded should not be retryable (systemic)"
    );

    let config_error = LlmRouterError::AgentOptionsConfigError {
        endpoint: "http://test:1234/v1".to_string(),
        details: "Invalid base_url".to_string(),
    };
    assert!(
        !config_error.is_retryable(),
        "AgentOptionsConfigError should not be retryable (systemic)"
    );
}

#[test]
fn test_llmroutererror_transient_errors_retryable() {
    // Verify all transient error variants return true for is_retryable()

    let stream_error = LlmRouterError::StreamError {
        endpoint: "http://test:1234/v1".to_string(),
        bytes_received: 42,
        error_message: "connection reset".to_string(),
    };
    assert!(
        stream_error.is_retryable(),
        "StreamError should be retryable (transient)"
    );

    let timeout = LlmRouterError::Timeout {
        endpoint: "http://test:1234/v1".to_string(),
        timeout_seconds: 30,
        attempt: 1,
        max_attempts: 3,
        router_tier: TargetModel::Balanced,
    };
    assert!(
        timeout.is_retryable(),
        "Timeout should be retryable (transient)"
    );
}

#[test]
fn test_llmroutererror_display_formatting() {
    // Verify error messages are clear and actionable

    let empty = LlmRouterError::EmptyResponse {
        endpoint: "http://test:1234/v1".to_string(),
    };
    assert!(
        empty.to_string().contains("empty response"),
        "EmptyResponse message should mention 'empty response'"
    );

    let unparseable = LlmRouterError::UnparseableResponse {
        endpoint: "http://test:1234/v1".to_string(),
        response: "BREAKFAST".to_string(),
        response_length: 9,
    };
    let msg = unparseable.to_string();
    assert!(
        msg.contains("unparseable") && msg.contains("BREAKFAST"),
        "UnparseableResponse should include 'unparseable' and actual response"
    );

    let size_exceeded = LlmRouterError::SizeExceeded {
        endpoint: "http://test:1234/v1".to_string(),
        size: 2048,
        max_size: 1024,
    };
    let msg = size_exceeded.to_string();
    assert!(
        msg.contains("2048") && msg.contains("1024"),
        "SizeExceeded should include actual and max sizes"
    );

    let timeout = LlmRouterError::Timeout {
        endpoint: "http://test:1234/v1".to_string(),
        timeout_seconds: 30,
        attempt: 2,
        max_attempts: 3,
        router_tier: TargetModel::Balanced,
    };
    let msg = timeout.to_string();
    assert!(
        msg.contains("30s") && msg.contains("2") && msg.contains("3"),
        "Timeout should include timeout duration and attempt numbers"
    );
}

#[test]
fn test_llmroutererror_converts_to_apperror() {
    // Verify LlmRouterError converts to AppError::LlmRouting variant
    //
    // This preserves type information for error classification instead of
    // losing it by converting to ModelQueryFailed.

    let router_error = LlmRouterError::UnparseableResponse {
        endpoint: "http://test:1234/v1".to_string(),
        response: "BAD".to_string(),
        response_length: 3,
    };

    let app_error: AppError = router_error.into();

    match app_error {
        AppError::LlmRouting(e) => match e {
            LlmRouterError::UnparseableResponse {
                endpoint,
                response,
                response_length,
            } => {
                assert_eq!(endpoint, "http://test:1234/v1");
                assert_eq!(response, "BAD");
                assert_eq!(response_length, 3);
            }
            _ => panic!("Expected UnparseableResponse variant"),
        },
        _ => panic!("Expected LlmRouting variant, got: {:?}", app_error),
    }
}

#[test]
fn test_llmroutererror_stream_error_includes_bytes_received() {
    // Verify StreamError tracks partial response size for debugging

    let stream_error = LlmRouterError::StreamError {
        endpoint: "http://test:1234/v1".to_string(),
        bytes_received: 512,
        error_message: "timeout".to_string(),
    };

    let msg = stream_error.to_string();
    assert!(
        msg.contains("512") && msg.contains("bytes received"),
        "StreamError should include bytes received for diagnostics: {}",
        msg
    );
}
