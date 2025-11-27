//! OpenAI-compatible chat completions handler
//!
//! Handles POST /v1/chat/completions requests (both streaming and non-streaming).

use crate::error::AppError;
use crate::handlers::AppState;
use crate::middleware::RequestId;
use crate::shared::query::{
    QueryConfig, execute_query_with_retry, query_model, record_routing_metrics,
};
use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderName, HeaderValue},
    response::{IntoResponse, Response},
};

use super::extractor::OpenAiJson;
use super::find_endpoint_by_name;
use super::types::{ChatCompletion, ChatCompletionRequest, ModelChoice, current_timestamp};

/// Custom header for surfacing non-fatal warnings to OpenAI API clients.
///
/// This header is added when the request succeeded but there were issues
/// that operators and clients should be aware of (e.g., health tracking failures).
///
/// The header value is a semicolon-separated list of warning messages.
/// Semicolons are used instead of commas since warning messages may contain commas.
pub const X_OCTOROUTE_WARNING: &str = "x-octoroute-warning";

/// Build a JSON response with optional warning header.
///
/// If warnings are present, adds an `X-Octoroute-Warning` header with a
/// semicolon-separated list of warning messages (truncated to 500 chars).
fn build_response_with_warnings<T: serde::Serialize>(body: T, warnings: &[String]) -> Response {
    let json_response = Json(body).into_response();

    if warnings.is_empty() {
        return json_response;
    }

    // Combine warnings into a single header value
    // Use semicolon as separator since commas might appear in messages
    let warning_value = warnings.join("; ");

    // Sanitize characters that are invalid in HTTP headers to preserve warning content.
    // HTTP headers require ASCII (RFC 7230 Section 3.2.6):
    // - Cannot contain control characters (0x00-0x1F except tab 0x09, and 0x7F)
    // - Cannot contain characters outside the visible ASCII range (> 0x7E)
    // Replace invalid characters with '?' to preserve message readability while ensuring
    // the header is valid. This prevents fallback to the generic error message.
    let warning_value: String = warning_value
        .chars()
        .map(|c| {
            if c.is_control() && c != ' ' {
                ' ' // Control characters (except space) -> space
            } else if !c.is_ascii() {
                '?' // Non-ASCII -> placeholder
            } else {
                c
            }
        })
        .collect();

    // Truncate to reasonable header length (500 chars) using single-pass iteration.
    // After non-ASCII sanitization above, all chars are ASCII (1 byte = 1 char),
    // so byte length equals char count. This avoids the double iteration of
    // .chars().count() followed by .chars().take().collect().
    let warning_value = if warning_value.len() > 500 {
        // Safe to truncate at byte boundary since all chars are now ASCII
        format!("{}...", &warning_value[..497])
    } else {
        warning_value
    };

    // Add the warning header to the response
    let (mut parts, body) = json_response.into_parts();
    if let Ok(header_value) = HeaderValue::from_str(&warning_value) {
        parts
            .headers
            .insert(HeaderName::from_static(X_OCTOROUTE_WARNING), header_value);
    } else {
        // Fallback should rarely be needed now that we sanitize control characters.
        // This catches any edge cases with non-ASCII or other invalid header characters.
        tracing::warn!(
            original_warning = %warning_value,
            warning_length = warning_value.len(),
            "Warning header contains invalid HTTP characters even after sanitization, using fallback. \
            Original warning logged for debugging."
        );
        parts.headers.insert(
            HeaderName::from_static(X_OCTOROUTE_WARNING),
            HeaderValue::from_static("health-tracking-degraded"),
        );
    }

    Response::from_parts(parts, body)
}

/// POST /v1/chat/completions handler
///
/// OpenAI-compatible chat completions endpoint. Supports:
/// - Model selection via tier names (auto, fast, balanced, deep) or specific model names
/// - Automatic task type inference for routing
/// - Both streaming (SSE) and non-streaming responses
///
/// # Model Selection
///
/// The `model` field can be:
/// - `"auto"` - Use LLM/hybrid routing to select tier
/// - `"fast"` / `"balanced"` / `"deep"` - Route directly to that tier
/// - Specific model name (e.g., `"qwen3-8b"`) - Find endpoint by name, bypass routing
///
/// # Response Format
///
/// **Non-streaming** (`stream: false` or omitted):
/// Returns OpenAI-compatible JSON response with:
/// - `id`: Unique completion ID
/// - `object`: "chat.completion"
/// - `created`: Unix timestamp
/// - `model`: Name of model used
/// - `choices`: Array with assistant message and finish_reason
/// - `usage`: Token usage statistics (estimated)
///
/// **Streaming** (`stream: true`):
/// Returns Server-Sent Events stream with chunks containing:
/// - Initial: role announcement (`delta.role: "assistant"`)
/// - Content: text deltas (`delta.content: "..."`)
/// - Finish: completion signal (`finish_reason: "stop"`)
/// - Done: `data: [DONE]`
pub async fn handler(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    OpenAiJson(request): OpenAiJson<ChatCompletionRequest>,
) -> Result<Response, AppError> {
    tracing::debug!(
        request_id = %request_id,
        model = ?request.model(),
        messages_count = request.messages().len(),
        stream = request.stream(),
        "Received chat completions request"
    );

    // Dispatch to streaming handler if requested
    if request.stream() {
        return super::streaming::handler(State(state), Extension(request_id), Json(request)).await;
    }

    // Convert messages to a single prompt for routing and query
    let prompt = request.to_prompt_string();
    let prompt_chars = prompt.chars().count();

    // Handle specific model requests differently - query the exact endpoint requested
    if let ModelChoice::Specific(name) = request.model() {
        // Find and use the specific endpoint (no tier selection)
        let (tier, endpoint) = find_endpoint_by_name(state.config(), name)?;

        tracing::info!(
            request_id = %request_id,
            model_name = %name,
            endpoint_name = %endpoint.name(),
            target_tier = ?tier,
            "Specific model selection - querying endpoint directly"
        );

        // Query the specific endpoint directly (no retry to different endpoints)
        let timeout_seconds = state.config().timeout_for_tier(tier);
        let content = query_model(&endpoint, &prompt, timeout_seconds, request_id, 1, 1).await?;

        // Mark endpoint as healthy on success, collect warnings
        let mut warnings: Vec<String> = Vec::new();
        if let Err(e) = state
            .selector()
            .health_checker()
            .mark_success(endpoint.name())
            .await
        {
            tracing::warn!(
                request_id = %request_id,
                endpoint_name = %endpoint.name(),
                error = %e,
                "Health tracking failed for specific model query"
            );
            // Record in metrics for observability parity with tier-based routing
            state
                .metrics()
                .health_tracking_failure(endpoint.name(), e.error_type());
            // Surface to client via warning header
            warnings.push(format!(
                "Health tracking failed: {} (endpoint health state may be stale)",
                e
            ));
        }

        let created = current_timestamp(Some(state.metrics().as_ref()), Some(&request_id));
        let response =
            ChatCompletion::new(content, endpoint.name().to_string(), prompt_chars, created);

        tracing::info!(
            request_id = %request_id,
            model = %response.model,
            response_length = response.choices[0].message.content().len(),
            warnings_count = warnings.len(),
            "Chat completion successful (specific model)"
        );

        return Ok(build_response_with_warnings(response, &warnings));
    }

    // For tier-based routing (auto, fast, balanced, deep)
    let decision = match request.model() {
        ModelChoice::Auto => {
            // Use router to determine tier (auto-detection)
            let metadata = request.to_route_metadata();
            let routing_start = std::time::Instant::now();
            let decision = state
                .router()
                .route(&prompt, &metadata, state.selector())
                .await?;
            let routing_duration_ms = routing_start.elapsed().as_secs_f64() * 1000.0;

            tracing::info!(
                request_id = %request_id,
                target_tier = ?decision.target(),
                routing_strategy = ?decision.strategy(),
                routing_duration_ms = %routing_duration_ms,
                "Routing decision made (auto)"
            );

            record_routing_metrics(&state, &decision, routing_duration_ms, request_id);
            decision
        }
        ModelChoice::Fast | ModelChoice::Balanced | ModelChoice::Deep => {
            // Direct tier selection (bypass routing)
            // Convert model choice to target tier - match arm guarantees this succeeds
            let tier = match request.model() {
                ModelChoice::Fast => crate::router::TargetModel::Fast,
                ModelChoice::Balanced => crate::router::TargetModel::Balanced,
                ModelChoice::Deep => crate::router::TargetModel::Deep,
                _ => unreachable!("outer match arm guarantees Fast/Balanced/Deep"),
            };
            let decision =
                crate::router::RoutingDecision::new(tier, crate::router::RoutingStrategy::Rule);

            tracing::info!(
                request_id = %request_id,
                target_tier = ?tier,
                "Direct tier selection (no routing)"
            );

            decision
        }
        ModelChoice::Specific(_) => unreachable!("handled above"),
    };

    // Execute query with retry logic (selects from tier)
    let config = QueryConfig::default();
    let result = execute_query_with_retry(&state, &decision, &prompt, request_id, &config).await?;

    // Use the endpoint that was actually selected
    let response_model = result.endpoint.name().to_string();

    // Build OpenAI-compatible response
    let created = current_timestamp(Some(state.metrics().as_ref()), Some(&request_id));
    let response = ChatCompletion::new(result.content, response_model, prompt_chars, created);

    tracing::info!(
        request_id = %request_id,
        model = %response.model,
        response_length = response.choices[0].message.content().len(),
        warnings_count = result.warnings.len(),
        "Chat completion successful"
    );

    // Return response with warning header if there were non-fatal issues
    Ok(build_response_with_warnings(response, &result.warnings))
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    // -------------------------------------------------------------------------
    // Warning Header Truncation Tests
    // -------------------------------------------------------------------------

    /// Helper to extract the warning sanitization and truncation logic for testing.
    /// Returns the sanitized and truncated warning string that would be used in the header.
    /// This mirrors the full logic in `build_response_with_warnings`:
    /// 1. Join warnings with "; "
    /// 2. Sanitize non-ASCII → '?' and control chars → ' '
    /// 3. Truncate to 500 bytes (safe because all chars are ASCII after sanitization)
    fn truncate_warning_for_header(warnings: &[String]) -> String {
        let warning_value = warnings.join("; ");

        // Sanitize (mirrors production): non-ASCII → '?', control → ' '
        let warning_value: String = warning_value
            .chars()
            .map(|c| {
                if c.is_control() && c != ' ' {
                    ' '
                } else if !c.is_ascii() {
                    '?'
                } else {
                    c
                }
            })
            .collect();

        // Truncate using byte length (safe because all chars are now ASCII)
        if warning_value.len() > 500 {
            format!("{}...", &warning_value[..497])
        } else {
            warning_value
        }
    }

    #[test]
    fn test_warning_truncation_ascii_only() {
        // ASCII-only string, truncation is safe at any byte boundary
        let long_warning = "a".repeat(600);
        let result = truncate_warning_for_header(&[long_warning]);

        assert_eq!(result.len(), 500); // 497 + "..."
        assert!(result.ends_with("..."));
        assert!(result.is_char_boundary(result.len())); // Valid UTF-8
    }

    #[test]
    fn test_warning_truncation_sanitizes_multibyte_to_placeholder() {
        // Multi-byte chars (like emoji) are sanitized to '?' BEFORE truncation.
        // This test verifies the sanitization + truncation pipeline.

        // Strategy: Fill with ASCII up to position 495, then add a 4-byte emoji
        let prefix = "x".repeat(495);
        let emoji = "🦑"; // 4 bytes in UTF-8, but becomes 1 char '?' after sanitization
        let suffix = "y".repeat(100);
        let warning = format!("{}{}{}", prefix, emoji, suffix);

        let result = truncate_warning_for_header(&[warning]);

        // The emoji should have been sanitized to '?'
        // Total: 495 + 1 + 100 = 596 chars (all ASCII after sanitization)
        // Should be truncated to 497 + "..." = 500 chars

        // Result must be valid UTF-8 (trivially true since all ASCII)
        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "Truncated warning must be valid UTF-8"
        );

        // Result should end with "..."
        assert!(
            result.ends_with("..."),
            "Truncated warning should end with ..."
        );

        // Result should be exactly 500 chars (byte length equals char count for ASCII)
        assert_eq!(
            result.len(),
            500,
            "Truncated warning should be exactly 500 bytes"
        );
        assert!(
            result.is_ascii(),
            "Result should be ASCII after sanitization"
        );
    }

    #[test]
    fn test_warning_truncation_chinese_characters_become_placeholders() {
        // Chinese characters are 3 bytes each in UTF-8, but get sanitized to '?'
        // After sanitization: 600 '?' chars = 600 bytes
        // Truncation: 497 + "..." = 500 bytes
        let chinese = "中".repeat(600); // 600 chars, 1800 bytes
        let result = truncate_warning_for_header(&[chinese]);

        // Must be valid UTF-8 (trivially true - all '?' and '.')
        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "Truncated text must be valid UTF-8"
        );

        assert!(result.ends_with("..."));
        // After sanitization: 600 '?' chars, truncated to 497 + "..." = 500 bytes
        assert_eq!(result.len(), 500);
        assert!(
            result.is_ascii(),
            "Result should be ASCII after sanitization"
        );
        // First 497 chars should all be '?'
        assert!(
            result[..497].chars().all(|c| c == '?'),
            "Chinese chars should become '?'"
        );
    }

    #[test]
    fn test_warning_truncation_mixed_multibyte_sanitizes_non_ascii() {
        // Mix of 1-byte (ASCII), 2-byte (é), 3-byte (中), and 4-byte (🦑) chars
        // Non-ASCII chars get sanitized to '?' before truncation
        let mixed = format!(
            "{}{}{}{}{}",
            "a".repeat(200),  // 200 chars, stays 'a'
            "é".repeat(150),  // 150 chars, becomes '?'
            "中".repeat(100), // 100 chars, becomes '?'
            "🦑".repeat(50),  // 50 chars, becomes '?'
            "z".repeat(50)    // 50 chars, stays 'z'
        ); // Total after sanitization: 550 ASCII chars

        assert!(
            mixed.chars().count() > 500,
            "Test setup: need >500 chars to trigger truncation"
        );

        let result = truncate_warning_for_header(&[mixed]);

        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "Truncated mixed text must be valid UTF-8"
        );
        assert!(result.ends_with("..."));
        // After sanitization, all 550 chars become ASCII, truncated to 500 bytes
        assert_eq!(result.len(), 500);
        assert!(
            result.is_ascii(),
            "Result should be ASCII after sanitization"
        );

        // Verify structure: first 200 'a', then some '?' (from é/中/🦑), then some 'z'
        assert!(
            result.starts_with(&"a".repeat(200)),
            "First 200 should be 'a'"
        );
        assert!(
            result[200..201].chars().all(|c| c == '?'),
            "After 'a's should be '?' from sanitized é"
        );
    }

    #[test]
    fn test_warning_under_limit_not_truncated() {
        let short = "This is a short warning";
        let result = truncate_warning_for_header(&[short.to_string()]);

        assert_eq!(result, short);
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn test_warning_exactly_500_chars_not_truncated() {
        let exactly_500 = "a".repeat(500);
        let result = truncate_warning_for_header(std::slice::from_ref(&exactly_500));

        assert_eq!(result, exactly_500);
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn test_warning_501_chars_gets_truncated() {
        let chars_501 = "a".repeat(501);
        let result = truncate_warning_for_header(&[chars_501]);

        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 500);
    }

    // -------------------------------------------------------------------------
    // Invalid Header Character Fallback Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_build_response_with_valid_warning_header() {
        use axum::http::HeaderName;

        let body = serde_json::json!({"test": "value"});
        let warnings = vec!["valid warning message".to_string()];

        let response = build_response_with_warnings(body, &warnings);

        let header = response
            .headers()
            .get(HeaderName::from_static(X_OCTOROUTE_WARNING));
        assert!(header.is_some(), "Warning header should be present");
        assert_eq!(header.unwrap().to_str().unwrap(), "valid warning message");
    }

    #[test]
    fn test_build_response_with_newline_sanitizes() {
        use axum::http::HeaderName;

        let body = serde_json::json!({"test": "value"});
        // Newline is invalid in HTTP headers - should be sanitized to space
        let warnings = vec!["warning with\nnewline".to_string()];

        let response = build_response_with_warnings(body, &warnings);

        let header = response
            .headers()
            .get(HeaderName::from_static(X_OCTOROUTE_WARNING));
        assert!(header.is_some(), "Warning header should still be present");
        // Newline should be replaced with space, preserving the message
        assert_eq!(header.unwrap().to_str().unwrap(), "warning with newline");
    }

    #[test]
    fn test_build_response_with_control_char_sanitizes() {
        use axum::http::HeaderName;

        let body = serde_json::json!({"test": "value"});
        // Null byte is invalid in HTTP headers - should be sanitized to space
        let warnings = vec!["warning with\x00null".to_string()];

        let response = build_response_with_warnings(body, &warnings);

        let header = response
            .headers()
            .get(HeaderName::from_static(X_OCTOROUTE_WARNING));
        assert!(header.is_some());
        // Control characters should be replaced with spaces, preserving the message
        assert_eq!(header.unwrap().to_str().unwrap(), "warning with null");
    }

    #[test]
    fn test_build_response_empty_warnings_no_header() {
        use axum::http::HeaderName;

        let body = serde_json::json!({"test": "value"});
        let warnings: Vec<String> = vec![];

        let response = build_response_with_warnings(body, &warnings);

        let header = response
            .headers()
            .get(HeaderName::from_static(X_OCTOROUTE_WARNING));
        assert!(header.is_none(), "No warning header when warnings empty");
    }

    #[test]
    fn test_build_response_with_non_ascii_sanitizes() {
        use axum::http::HeaderName;

        let body = serde_json::json!({"test": "value"});
        // Non-ASCII characters (Chinese and emoji) should be sanitized to preserve message
        let warnings = vec!["Health check failed for 北京-server 🦑".to_string()];

        let response = build_response_with_warnings(body, &warnings);

        let header = response
            .headers()
            .get(HeaderName::from_static(X_OCTOROUTE_WARNING));
        assert!(header.is_some(), "Warning header should be present");

        let value = header.unwrap().to_str().unwrap();
        // Should NOT fall back to generic message - should preserve content
        assert_ne!(
            value, "health-tracking-degraded",
            "Should not fall back to generic message, should sanitize non-ASCII"
        );
        // Should contain the sanitized message
        assert!(
            value.contains("Health check failed"),
            "Should preserve ASCII content. Got: {}",
            value
        );
    }

    #[test]
    fn test_build_response_with_emoji_sanitizes() {
        use axum::http::HeaderName;

        let body = serde_json::json!({"test": "value"});
        // Pure emoji should be sanitized (each emoji replaced with placeholder)
        let warnings = vec!["Error 🔴 warning 🟡 info 🟢".to_string()];

        let response = build_response_with_warnings(body, &warnings);

        let header = response
            .headers()
            .get(HeaderName::from_static(X_OCTOROUTE_WARNING));
        assert!(header.is_some());

        let value = header.unwrap().to_str().unwrap();
        // Should preserve ASCII content
        assert!(
            value.contains("Error"),
            "Should preserve 'Error'. Got: {}",
            value
        );
        assert!(
            value.contains("warning"),
            "Should preserve 'warning'. Got: {}",
            value
        );
        assert!(
            value.contains("info"),
            "Should preserve 'info'. Got: {}",
            value
        );
    }
}
