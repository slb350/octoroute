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

    // Sanitize control characters to preserve warning content
    // HTTP headers cannot contain control characters (0x00-0x1F except tab, and 0x7F)
    // Replace them with spaces to preserve message readability
    let warning_value: String = warning_value
        .chars()
        .map(|c| if c.is_control() && c != ' ' { ' ' } else { c })
        .collect();

    // Truncate to reasonable header length (500 chars)
    // Use char-based truncation to avoid splitting multi-byte UTF-8 characters
    let warning_value = if warning_value.chars().count() > 500 {
        format!("{}...", warning_value.chars().take(497).collect::<String>())
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

    /// Helper to extract the warning truncation logic for testing.
    /// Returns the truncated warning string that would be used in the header.
    /// This mirrors the logic in `build_response_with_warnings`.
    fn truncate_warning_for_header(warnings: &[String]) -> String {
        let warning_value = warnings.join("; ");

        // Use char-based truncation to avoid splitting multi-byte UTF-8 characters
        if warning_value.chars().count() > 500 {
            format!("{}...", warning_value.chars().take(497).collect::<String>())
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
    fn test_warning_truncation_preserves_utf8_multibyte_chars() {
        // Create a string where byte position 497 falls in the middle of a multi-byte char
        // The emoji "🦑" (octopus) is 4 bytes in UTF-8: F0 9F A6 91
        // We want the truncation point (497) to fall inside a multi-byte character

        // Strategy: Fill with ASCII up to position 495, then add a 4-byte emoji
        // Bytes 0-494: ASCII (495 bytes)
        // Bytes 495-498: emoji (4 bytes) - truncation at 497 would split this!
        let prefix = "x".repeat(495);
        let emoji = "🦑"; // 4 bytes
        let suffix = "y".repeat(100);
        let warning = format!("{}{}{}", prefix, emoji, suffix);

        // Verify our setup: byte 497 should be inside the emoji
        assert!(
            !warning.is_char_boundary(497),
            "Test setup failed: byte 497 should NOT be a char boundary"
        );

        let result = truncate_warning_for_header(&[warning]);

        // The result MUST be valid UTF-8 (this is the key assertion)
        // If truncation splits a multi-byte char, this will be invalid
        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "Truncated warning must be valid UTF-8"
        );

        // Result should end with "..."
        assert!(
            result.ends_with("..."),
            "Truncated warning should end with ..."
        );

        // Result should not exceed 500 chars (but may be shorter due to char-safe truncation)
        assert!(
            result.chars().count() <= 500,
            "Truncated warning should not exceed 500 chars"
        );
    }

    #[test]
    fn test_warning_truncation_chinese_characters() {
        // Chinese characters are 3 bytes each in UTF-8
        // This tests that we handle 3-byte sequences correctly
        // Need >500 chars to trigger truncation (not bytes)
        let chinese = "中".repeat(600); // 600 chars, 1800 bytes
        let result = truncate_warning_for_header(&[chinese]);

        // Must be valid UTF-8
        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "Truncated Chinese text must be valid UTF-8"
        );

        assert!(result.ends_with("..."));
        // 497 Chinese chars + "..." = 500 chars displayed
        assert_eq!(result.chars().count(), 500);
    }

    #[test]
    fn test_warning_truncation_mixed_multibyte() {
        // Mix of 1-byte (ASCII), 2-byte (é), 3-byte (中), and 4-byte (🦑) chars
        // Need >500 chars total to trigger truncation
        let mixed = format!(
            "{}{}{}{}{}",
            "a".repeat(200),  // 200 chars
            "é".repeat(150),  // 150 chars
            "中".repeat(100), // 100 chars
            "🦑".repeat(50),  // 50 chars
            "z".repeat(50)    // 50 chars
        ); // Total: 550 chars

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
        assert_eq!(result.chars().count(), 500);
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
}
