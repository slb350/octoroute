//! Error classification tests

use super::*;
use crate::error::AppError;

#[test]
fn test_systemic_errors_are_not_retryable() {
    // Parse failures and LLM misconfiguration are systemic errors
    let systemic_errors = vec![
        AppError::LlmRouting(LlmRouterError::UnparseableResponse {
            endpoint: "test".to_string(),
            response: "invalid response".to_string(),
            response_length: 100,
        }),
        AppError::LlmRouting(LlmRouterError::EmptyResponse {
            endpoint: "test".to_string(),
        }),
        AppError::LlmRouting(LlmRouterError::SizeExceeded {
            endpoint: "test".to_string(),
            size: 2000,
            max_size: 1024,
        }),
        AppError::LlmRouting(LlmRouterError::Refusal {
            endpoint: "test".to_string(),
            message: "refusal response".to_string(),
        }),
        AppError::LlmRouting(LlmRouterError::AgentOptionsConfigError {
            endpoint: "test".to_string(),
            details: "invalid config".to_string(),
        }),
        AppError::Config("Invalid configuration".to_string()),
    ];

    for error in systemic_errors {
        assert!(
            !LlmBasedRouter::is_retryable_error(&error),
            "Error should be systemic (not retryable): {:?}",
            error
        );
    }
}

#[test]
fn test_transient_errors_are_retryable() {
    // Network failures and timeouts are transient errors
    let transient_errors = vec![
        AppError::LlmRouting(LlmRouterError::Timeout {
            endpoint: "test".to_string(),
            timeout_seconds: 10,
            attempt: 1,
            max_attempts: 3,
            router_tier: crate::router::TargetModel::Balanced,
        }),
        AppError::LlmRouting(LlmRouterError::StreamError {
            endpoint: "test".to_string(),
            bytes_received: 0,
            error_message: "connection refused".to_string(),
        }),
        AppError::LlmRouting(LlmRouterError::StreamError {
            endpoint: "test".to_string(),
            bytes_received: 100,
            error_message: "network timeout".to_string(),
        }),
        AppError::RoutingFailed("No healthy endpoints available".to_string()),
    ];

    for error in transient_errors {
        assert!(
            LlmBasedRouter::is_retryable_error(&error),
            "Error should be transient (retryable): {:?}",
            error
        );
    }
}

#[test]
fn test_error_classification_is_type_based() {
    // Type-safe error classification - no string matching needed
    let systemic_errors = vec![
        AppError::LlmRouting(LlmRouterError::UnparseableResponse {
            endpoint: "test".to_string(),
            response: "UNPARSEABLE RESPONSE".to_string(),
            response_length: 100,
        }),
        AppError::LlmRouting(LlmRouterError::EmptyResponse {
            endpoint: "test".to_string(),
        }),
        AppError::LlmRouting(LlmRouterError::SizeExceeded {
            endpoint: "test".to_string(),
            size: 2000,
            max_size: 1024,
        }),
    ];

    for error in systemic_errors {
        assert!(
            !LlmBasedRouter::is_retryable_error(&error),
            "Error should be systemic (not retryable) based on type: {:?}",
            error
        );
    }
}

#[test]
fn test_agent_options_build_failure_is_systemic() {
    // GAP #2: AgentOptions Build Failure
    //
    // If AgentOptions::builder().build() fails (e.g., invalid configuration),
    // the error should be classified as systemic (not retryable).
    // Retrying the same bad configuration 3 times is wasteful - should fail fast.
    //
    // The error message format is: "Failed to configure AgentOptions: {error} (...)"
    // This test verifies it's classified as systemic.

    let config_error = AppError::LlmRouting(LlmRouterError::AgentOptionsConfigError {
        endpoint: "http://localhost:1234/v1".to_string(),
        details: "invalid model name (model=bad-model, max_tokens=4096)".to_string(),
    });

    assert!(
        !LlmBasedRouter::is_retryable_error(&config_error),
        "AgentOptions build failures should be systemic (not retryable) to avoid wasted retries"
    );
}

#[test]
fn test_empty_stream_is_systemic() {
    // GAP #5: Empty Stream (No ContentBlock Items)
    //
    // If stream completes successfully but yields zero ContentBlock items,
    // response_text will be empty and parse_routing_decision("") is called.
    // This should return an error with "Router LLM returned empty response".
    //
    // This test verifies:
    // 1. Empty response error is classified as systemic (not retryable)
    // 2. Error message clearly indicates the problem

    let empty_response_error = AppError::LlmRouting(LlmRouterError::EmptyResponse {
        endpoint: "router".to_string(),
    });

    // Should be classified as systemic
    assert!(
        !LlmBasedRouter::is_retryable_error(&empty_response_error),
        "Empty stream responses should be systemic (not retryable) - indicates LLM malfunction"
    );

    // Also verify parse_routing_decision returns this error for empty input
    let result = LlmBasedRouter::parse_routing_decision("");
    assert!(
        result.is_err(),
        "parse_routing_decision should fail on empty string"
    );

    match result {
        Err(AppError::LlmRouting(LlmRouterError::EmptyResponse { .. })) => {
            // Error type is correct
        }
        other => panic!("Expected LlmRouting(EmptyResponse) error, got: {:?}", other),
    }
}

#[test]
fn test_size_limit_exceeded_is_systemic() {
    // GAP #4: MAX_ROUTER_RESPONSE Boundary Conditions
    //
    // When router response exceeds MAX_ROUTER_RESPONSE (1024 bytes), the error
    // should be classified as systemic (not retryable) because it indicates
    // LLM malfunction or misconfiguration.
    //
    // Boundary check in try_router_query():
    //   if response_text.len() + text_block.text.len() > MAX_ROUTER_RESPONSE
    //
    // This means:
    //   - Exactly 1024 bytes: PASSES (1024 > 1024 is false)
    //   - 1025 bytes: FAILS (1025 > 1024 is true)

    let size_exceeded_error = AppError::LlmRouting(LlmRouterError::SizeExceeded {
        endpoint: "http://localhost:1234/v1".to_string(),
        size: 1025,
        max_size: 1024,
    });

    // Should be classified as systemic (pattern "exceeded" in systemic_patterns)
    assert!(
        !LlmBasedRouter::is_retryable_error(&size_exceeded_error),
        "Size limit exceeded should be systemic (not retryable) - indicates LLM malfunction"
    );
}

#[test]
fn test_max_router_response_boundary_logic() {
    // GAP #4: MAX_ROUTER_RESPONSE Boundary Conditions (detailed)
    //
    // Documents the exact boundary behavior:
    //   current_len + incoming_len > MAX_ROUTER_RESPONSE
    //
    // Edge cases:
    //   1. current=0, incoming=1024  → 1024 > 1024 = false → ACCEPT
    //   2. current=0, incoming=1025  → 1025 > 1024 = true  → REJECT
    //   3. current=1020, incoming=4  → 1024 > 1024 = false → ACCEPT
    //   4. current=1020, incoming=5  → 1025 > 1024 = true  → REJECT
    //   5. current=512, incoming=512 → 1024 > 1024 = false → ACCEPT (multiple chunks)

    use super::MAX_ROUTER_RESPONSE;

    // Verify the constant value
    assert_eq!(MAX_ROUTER_RESPONSE, 1024, "Limit should be 1KB");

    // Simulate boundary checks (logic from try_router_query)
    let test_cases = vec![
        (0, 1024, false, "Single chunk at limit should pass"),
        (0, 1025, true, "Single chunk over limit should fail"),
        (1020, 4, false, "Total exactly 1024 should pass"),
        (1020, 5, true, "Total 1025 should fail"),
        (512, 512, false, "Two chunks totaling 1024 should pass"),
        (512, 513, true, "Two chunks totaling 1025 should fail"),
    ];

    for (current_len, incoming_len, should_reject, description) in test_cases {
        let would_exceed = current_len + incoming_len > MAX_ROUTER_RESPONSE;
        assert_eq!(
            would_exceed,
            should_reject,
            "{}: current={}, incoming={}, total={}",
            description,
            current_len,
            incoming_len,
            current_len + incoming_len
        );
    }
}

#[test]
fn test_max_router_response_limit_is_reasonable() {
    // Test Gap #3: Response Truncation at 1KB Limit
    //
    // Documents that MAX_ROUTER_RESPONSE is set to prevent unbounded memory growth
    // Expected router responses: "FAST", "BALANCED", or "DEEP" (~10 bytes)
    // 1KB limit is 100x the expected size - exceeding it indicates LLM malfunction
    //
    // Note: The actual enforcement happens in try_router_query() during streaming
    // (checked during while let Some(result) = stream.next() loop). When exceeded,
    // it returns SizeExceeded error instead of truncating and continuing to parse.

    use super::MAX_ROUTER_RESPONSE;

    // Verify limit is reasonable (not too small, not too large)
    assert_eq!(MAX_ROUTER_RESPONSE, 1024, "Should be 1KB");

    // Verify this is much larger than expected responses
    let expected_response_size = "BALANCED".len(); // ~8 bytes
    assert!(
        MAX_ROUTER_RESPONSE > expected_response_size * 100,
        "Limit should be 100x+ larger than expected response"
    );

    // Note: The limit is also small enough to prevent OOM attacks (1KB < 1MB)
    // This is verified by the assert_eq! above confirming MAX_ROUTER_RESPONSE == 1024
}

#[test]
fn test_stream_error_with_partial_response_is_retryable() {
    // GAP #6: Stream Timeout/Error After Partial Response
    //
    // Scenario: Stream yields partial data (e.g., "BA" from "BALANCED") then
    // encounters an error (timeout, connection lost, etc.).
    //
    // The error message format is "Stream error after X bytes received: <error>"
    // (see try_router_query stream error handling in the Err(e) match arm).
    //
    // This should be classified as RETRYABLE (transient network/endpoint issue),
    // NOT systemic (LLM malfunction). The LLM was working correctly - the
    // network/endpoint failed mid-stream.
    //
    // Verifies:
    // 1. Stream errors are not in systemic patterns
    // 2. Classification is correct regardless of partial data amount
    // 3. Underlying error details don't affect retryability

    // Simulate stream error after receiving partial response
    let stream_error_partial = AppError::LlmRouting(LlmRouterError::StreamError {
        endpoint: "http://localhost:1234/v1".to_string(),
        bytes_received: 2,
        error_message: "connection timeout".to_string(),
    });

    assert!(
        LlmBasedRouter::is_retryable_error(&stream_error_partial),
        "Stream errors should be retryable (transient network issue), even with partial data"
    );

    // Also test with zero bytes received
    let stream_error_immediate = AppError::LlmRouting(LlmRouterError::StreamError {
        endpoint: "http://localhost:1234/v1".to_string(),
        bytes_received: 0,
        error_message: "connection refused".to_string(),
    });

    assert!(
        LlmBasedRouter::is_retryable_error(&stream_error_immediate),
        "Stream errors should be retryable even if no data was received"
    );

    // Test with various underlying error messages
    let stream_error_timeout = AppError::LlmRouting(LlmRouterError::StreamError {
        endpoint: "http://localhost:1234/v1".to_string(),
        bytes_received: 15,
        error_message: "timed out".to_string(),
    });

    assert!(
        LlmBasedRouter::is_retryable_error(&stream_error_timeout),
        "Stream timeout errors should be retryable regardless of timeout wording"
    );
}
