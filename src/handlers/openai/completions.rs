//! OpenAI-compatible chat completions handler
//!
//! Handles POST /v1/chat/completions requests (both streaming and non-streaming).

use crate::error::AppError;
use crate::handlers::AppState;
use crate::middleware::RequestId;
use crate::shared::query::{QueryConfig, execute_query_with_retry, record_routing_metrics};
use axum::{
    Extension, Json,
    extract::State,
    response::{IntoResponse, Response},
};

use super::find_endpoint_by_name;
use super::types::{ChatCompletion, ChatCompletionRequest, ModelChoice};

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

    // Determine routing based on model choice
    let (decision, model_name) = match request.model() {
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
            (decision, None)
        }
        ModelChoice::Fast | ModelChoice::Balanced | ModelChoice::Deep => {
            // Direct tier selection (bypass routing)
            let tier = request.model().to_target_model().unwrap();
            let decision =
                crate::router::RoutingDecision::new(tier, crate::router::RoutingStrategy::Rule);

            tracing::info!(
                request_id = %request_id,
                target_tier = ?tier,
                "Direct tier selection (no routing)"
            );

            (decision, None)
        }
        ModelChoice::Specific(name) => {
            // Find endpoint by name (bypass routing entirely)
            let (tier, endpoint) = find_endpoint_by_name(state.config(), name)?;
            let decision =
                crate::router::RoutingDecision::new(tier, crate::router::RoutingStrategy::Rule);

            tracing::info!(
                request_id = %request_id,
                model_name = %name,
                target_tier = ?tier,
                "Specific model selection (routing bypassed)"
            );

            (decision, Some(endpoint.name().to_string()))
        }
    };

    // Execute query with retry logic
    let config = QueryConfig::default();
    let result = execute_query_with_retry(&state, &decision, &prompt, request_id, &config).await?;

    // Determine model name for response
    let response_model = model_name.unwrap_or_else(|| result.endpoint.name().to_string());

    // Build OpenAI-compatible response
    let response = ChatCompletion::new(result.content, response_model, prompt_chars);

    tracing::info!(
        request_id = %request_id,
        model = %response.model,
        response_length = response.choices[0].message.content.len(),
        "Chat completion successful"
    );

    Ok(Json(response).into_response())
}
