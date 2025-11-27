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

use super::find_endpoint_by_name;
use super::types::{ChatCompletion, ChatCompletionRequest, ModelChoice, current_timestamp};

/// Custom header for surfacing non-fatal warnings to OpenAI API clients.
///
/// This header is added when the request succeeded but there were issues
/// that operators and clients should be aware of (e.g., health tracking failures).
///
/// The header value is a comma-separated list of warning codes/messages.
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

    // Truncate to reasonable header length (500 chars)
    let warning_value = if warning_value.len() > 500 {
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
        // If the warning contains invalid header characters, use a generic message
        // Note: from_static only accepts compile-time constants, which are always valid
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
    Json(request): Json<ChatCompletionRequest>,
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
            response_length = response.choices[0].message.content.len(),
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
        response_length = response.choices[0].message.content.len(),
        warnings_count = result.warnings.len(),
        "Chat completion successful"
    );

    // Return response with warning header if there were non-fatal issues
    Ok(build_response_with_warnings(response, &result.warnings))
}
