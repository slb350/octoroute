//! OpenAI-compatible streaming chat completions handler
//!
//! Handles POST /v1/chat/completions requests with stream: true.
//!
//! # Limitations
//!
//! **Retry Logic**: Unlike non-streaming requests, streaming does not support
//! automatic retry on endpoint failure. If the selected endpoint fails, the
//! stream returns an error event and terminates. This is because SSE streams
//! cannot be restarted once the HTTP response headers are sent.
//!
//! **Health Tracking**: Initial query failures are tracked for endpoint health.
//! Mid-stream failures are logged and reported to the client but do not affect
//! health tracking, as they typically indicate transient network issues rather
//! than endpoint health problems.
//!
//! # Serialization Safety
//!
//! `ChatCompletionChunk` serialization uses the `serialize_chunk` helper which
//! handles errors gracefully instead of panicking. While serialization should
//! never fail for this simple struct (String, i64, Option<String> types), we
//! handle the theoretical failure case by:
//!
//! 1. Logging the error for production visibility
//! 2. Returning a hand-crafted JSON error event to the client
//!
//! This ensures the streaming handler never panics, even in edge cases like
//! OOM during serialization. Property-based tests in `tests/openai_streaming.rs`
//! verify serialization succeeds for all valid inputs.

use crate::error::AppError;
use crate::handlers::AppState;
use crate::metrics::Metrics;
use crate::middleware::RequestId;
use crate::models::ModelSelector;
use crate::shared::query::record_routing_metrics;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::find_endpoint_by_name;
use axum::{
    Extension, Json,
    extract::State,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::stream::{self, StreamExt};
use std::convert::Infallible;
use std::time::Duration;

use super::types::{
    ChatCompletionChunk, ChatCompletionRequest, ModelChoice, TimestampResult, current_timestamp,
};

/// Serialize a chunk to JSON, returning a fallback error event on failure.
///
/// This helper handles the unlikely case where `ChatCompletionChunk` serialization
/// fails (should never happen with simple types, but we handle it gracefully).
///
/// On failure:
/// 1. Logs the error with request context
/// 2. Returns a hand-crafted JSON error event that is guaranteed valid
///
/// Note: We don't set `error_occurred` here because the caller may want to
/// continue streaming (e.g., for initial chunks). The caller should set it
/// if appropriate for their context.
fn serialize_chunk(chunk: &ChatCompletionChunk, request_id: &RequestId) -> String {
    match serde_json::to_string(chunk) {
        Ok(json) => json,
        Err(e) => {
            // This should never happen - ChatCompletionChunk contains only simple types.
            // Log as error so it's visible in production monitoring.
            tracing::error!(
                request_id = %request_id,
                error = %e,
                chunk_id = %chunk.id,
                "BUG: ChatCompletionChunk serialization failed. This indicates a bug in the \
                struct definition (e.g., non-serializable field added). Returning error event."
            );
            // Return a hand-crafted JSON that we know is valid
            format!(
                r#"{{"error":"serialization_failed","request_id":"{}","chunk_id":"{}"}}"#,
                request_id, chunk.id
            )
        }
    }
}

/// POST /v1/chat/completions handler for streaming requests
///
/// Returns Server-Sent Events (SSE) stream of chat completion chunks.
///
/// # SSE Format
///
/// Each event is formatted as:
/// ```text
/// data: {"id":"...","object":"chat.completion.chunk",...}
///
/// ```
///
/// The stream ends with:
/// ```text
/// data: [DONE]
///
/// ```
///
/// # Chunk Types
///
/// 1. Initial chunk: role announcement (`delta.role: "assistant"`)
/// 2. Content chunks: text deltas (`delta.content: "..."`)
/// 3. Finish chunk: completion signal (`finish_reason: "stop"`)
pub async fn handler(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Response, AppError> {
    tracing::debug!(
        request_id = %request_id,
        model = ?request.model(),
        messages_count = request.messages().len(),
        "Received streaming chat completions request"
    );

    // Convert messages to a single prompt for routing and query
    let prompt = request.to_prompt_string();

    // Handle specific model requests differently - use the exact endpoint requested
    // Track tier for metrics recording (both specific and tier-based paths)
    let (endpoint, target_tier) = if let ModelChoice::Specific(name) = request.model() {
        // Find and use the specific endpoint directly (no tier selection)
        let (tier, endpoint) = find_endpoint_by_name(state.config(), name)?;

        tracing::info!(
            request_id = %request_id,
            model_name = %name,
            endpoint_name = %endpoint.name(),
            target_tier = ?tier,
            "Specific model selection - streaming directly to endpoint"
        );

        (endpoint, tier)
    } else {
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
                    "Routing decision made (auto, streaming)"
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
                    "Direct tier selection (streaming)"
                );

                // Record metrics for observability parity with auto-routing
                // Duration is 0.0 since no actual routing computation happens
                record_routing_metrics(&state, &decision, 0.0, request_id);
                decision
            }
            ModelChoice::Specific(_) => unreachable!("handled above"),
        };

        // Select endpoint from target tier
        let failed_endpoints = crate::models::ExclusionSet::new();
        let endpoint = state
            .selector()
            .select(decision.target(), &failed_endpoints)
            .await
            .ok_or_else(|| {
                AppError::RoutingFailed(format!(
                    "No available healthy endpoints for tier {:?}",
                    decision.target()
                ))
            })?
            .clone();
        (endpoint, decision.target())
    };

    // Build AgentOptions
    let options = open_agent::AgentOptions::builder()
        .model(endpoint.name())
        .base_url(endpoint.base_url())
        .max_tokens(endpoint.max_tokens() as u32)
        .temperature(endpoint.temperature() as f32)
        .build()
        .map_err(|e| {
            tracing::error!(
                request_id = %request_id,
                endpoint_name = %endpoint.name(),
                error = %e,
                "Failed to build AgentOptions for streaming"
            );
            AppError::ModelQuery(crate::error::ModelQueryError::AgentOptionsConfigError {
                endpoint: endpoint.base_url().to_string(),
                details: format!("{}", e),
            })
        })?;

    // Generate unique ID and timestamp for this completion
    let completion_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let TimestampResult {
        timestamp: created,
        warning: clock_warning,
    } = current_timestamp(Some(state.metrics().as_ref()), Some(&request_id));
    // For streaming, we can't add warnings to headers (already sent).
    // Log the warning if present - metric is already recorded in current_timestamp.
    if let Some(w) = clock_warning {
        tracing::warn!(request_id = %request_id, warning = %w, "Clock error during streaming");
    }
    let response_model = endpoint.name().to_string();

    tracing::info!(
        request_id = %request_id,
        completion_id = %completion_id,
        model = %response_model,
        endpoint_name = %endpoint.name(),
        "Starting streaming response"
    );

    // Create the SSE stream (pass selector for health tracking, metrics for observability)
    // Model invocation is recorded inside create_sse_stream on success only
    let stream = create_sse_stream(
        prompt,
        options,
        completion_id,
        response_model,
        created,
        request_id,
        endpoint.name().to_string(),
        target_tier,
        state.selector_arc(),
        state.metrics(),
    );

    Ok(Sse::new(stream)
        .keep_alive(
            KeepAlive::new().interval(Duration::from_secs(15)).text(":"), // SSE comment for keep-alive (axum adds newlines)
        )
        .into_response())
}

/// Create an SSE stream from the model query
///
/// # Note on Health Tracking
///
/// Health tracking is performed for initial query failures only. Mid-stream
/// failures are logged and reported to the client but do not affect health
/// tracking, as they typically indicate transient issues rather than endpoint
/// health problems.
///
/// Health tracking failures are recorded in metrics for observability parity
/// with the non-streaming handler.
#[allow(clippy::too_many_arguments)] // Needed for health tracking and metrics
fn create_sse_stream(
    prompt: String,
    options: open_agent::AgentOptions,
    completion_id: String,
    model: String,
    created: i64,
    request_id: RequestId,
    endpoint_name: String,
    target_tier: crate::router::TargetModel,
    selector: Arc<ModelSelector>,
    metrics: Arc<Metrics>,
) -> impl futures::Stream<Item = Result<Event, Infallible>> {
    stream::once(async move {
        // Start the model query
        let model_stream = match open_agent::query(&prompt, &options).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(
                    request_id = %request_id,
                    endpoint_name = %endpoint_name,
                    error = %e,
                    "Failed to start streaming query"
                );

                // Mark endpoint as failed for health tracking
                if let Err(health_err) = selector.health_checker().mark_failure(&endpoint_name).await
                {
                    tracing::warn!(
                        request_id = %request_id,
                        endpoint_name = %endpoint_name,
                        error = %health_err,
                        "Health tracking failed for streaming query failure"
                    );
                    // Record in metrics for observability parity with non-streaming handler
                    metrics.health_tracking_failure(&endpoint_name, health_err.error_type());
                }

                // Return an error event (sanitized - don't expose internal error details)
                // Include request ID for support correlation
                let error_chunk = ChatCompletionChunk::content(
                    &completion_id,
                    &model,
                    created,
                    &format!("[Error: Failed to start model query. Request ID: {}. Please retry.]", request_id),
                );
                return stream::iter(vec![
                    Ok(Event::default().data(serialize_chunk(&error_chunk, &request_id))),
                    Ok(Event::default().data("[DONE]")),
                ])
                .boxed();
            }
        };

        // Create initial chunk with role announcement
        let initial = ChatCompletionChunk::initial(&completion_id, &model, created);
        let initial_event = Ok(Event::default().data(serialize_chunk(&initial, &request_id)));

        // Save request_id for use after the stream closure
        let request_id_for_finish = request_id;

        // Track if an error occurs during streaming
        // If error occurs, we skip the finish_reason: "stop" chunk since it's misleading
        //
        // NOTE ON THREAD SAFETY: There is NO race condition here despite streams being
        // "created upfront". The `.chain()` operator ensures sequential execution:
        // - content_stream must be exhausted before finish_events is polled
        // - finish_events must be exhausted before success_tracker is polled
        // The async blocks inside stream::once() don't execute until polled, so
        // error_occurred.load() in success_tracker will always see any .store() from
        // content_stream. SeqCst ordering provides the memory visibility guarantee.
        let error_occurred = Arc::new(AtomicBool::new(false));

        // Map model stream to SSE events
        let content_stream = model_stream
            .filter_map({
                let completion_id = completion_id.clone();
                let model = model.clone();
                let request_id = request_id;
                let endpoint_name = endpoint_name.clone();
                let error_occurred = error_occurred.clone();
                let metrics = metrics.clone();
                move |result| {
                    let completion_id = completion_id.clone();
                    let model = model.clone();
                    let request_id = request_id;
                    let endpoint_name = endpoint_name.clone();
                    let error_occurred = error_occurred.clone();
                    let metrics = metrics.clone();
                    async move {
                        match result {
                            Ok(block) => {
                                use open_agent::ContentBlock;
                                match block {
                                    ContentBlock::Text(text_block) => {
                                        let chunk = ChatCompletionChunk::content(
                                            &completion_id,
                                            &model,
                                            created,
                                            &text_block.text,
                                        );
                                        Some(Ok(Event::default().data(
                                            serialize_chunk(&chunk, &request_id),
                                        )))
                                    }
                                    other_block => {
                                        // Log warning for non-text blocks (consistent with non-streaming)
                                        tracing::warn!(
                                            request_id = %request_id,
                                            endpoint_name = %endpoint_name,
                                            block_type = ?other_block,
                                            "Received non-text content block, skipping (not supported - text blocks only)"
                                        );
                                        None
                                    }
                                }
                            }
                            Err(e) => {
                                // Mark that an error occurred so we skip the misleading finish chunk
                                error_occurred.store(true, Ordering::SeqCst);

                                // Record metric for observability (doesn't affect health tracking)
                                metrics.mid_stream_failure(&endpoint_name);

                                // Propagate error to client instead of silent drop
                                tracing::error!(
                                    request_id = %request_id,
                                    endpoint_name = %endpoint_name,
                                    error = %e,
                                    "Stream error during content delivery - notifying client"
                                );
                                // Send error indication to client (sanitized message)
                                // NOTE: SSE data fields cannot contain newlines - removed \n\n prefix
                                // Include request ID for support correlation
                                let error_chunk = ChatCompletionChunk::content(
                                    &completion_id,
                                    &model,
                                    created,
                                    &format!("[Stream Error: Content may be incomplete. Request ID: {}. Please retry.]", request_id),
                                );
                                Some(Ok(Event::default().data(
                                    serialize_chunk(&error_chunk, &request_id),
                                )))
                            }
                        }
                    }
                }
            })
            .boxed();

        // Create finish events - skip finish_reason: "stop" if error occurred
        // Sending finish_reason: "stop" after an error is semantically incorrect
        let finish_chunk = ChatCompletionChunk::finish(&completion_id, &model, created);
        let finish_events = {
            let error_occurred = error_occurred.clone();
            let request_id = request_id_for_finish;
            stream::once(async move {
                if error_occurred.load(Ordering::SeqCst) {
                    // Error occurred - only send [DONE], skip misleading finish_reason: "stop"
                    tracing::debug!(
                        request_id = %request_id,
                        "Skipping finish chunk due to stream error"
                    );
                    vec![Ok(Event::default().data("[DONE]"))]
                } else {
                    // Normal completion - send finish chunk then [DONE]
                    vec![
                        Ok(Event::default().data(serialize_chunk(&finish_chunk, &request_id))),
                        Ok(Event::default().data("[DONE]")),
                    ]
                }
            })
            .flat_map(stream::iter)
        };

        // Mark endpoint as healthy and record model invocation when stream completes successfully
        // This runs after all events are sent, providing symmetric health/metrics tracking
        // with the non-streaming handler (completions.rs)
        // Only mark success and record metrics if no error occurred during streaming
        let success_tracker = {
            let selector = selector.clone();
            let metrics = metrics.clone();
            let endpoint_name = endpoint_name.clone();
            let request_id = request_id_for_finish;
            let error_occurred = error_occurred.clone();
            stream::once(async move {
                // Only mark success and record metrics if no error occurred
                if error_occurred.load(Ordering::SeqCst) {
                    tracing::debug!(
                        request_id = %request_id,
                        endpoint_name = %endpoint_name,
                        "Skipping health/metrics tracking due to stream error"
                    );
                } else {
                    // Record model invocation only on success (parity with non-streaming handler)
                    let tier_enum = match target_tier {
                        crate::router::TargetModel::Fast => crate::metrics::Tier::Fast,
                        crate::router::TargetModel::Balanced => crate::metrics::Tier::Balanced,
                        crate::router::TargetModel::Deep => crate::metrics::Tier::Deep,
                    };
                    if let Err(e) = metrics.record_model_invocation(tier_enum) {
                        metrics.metrics_recording_failure("record_model_invocation");
                        tracing::error!(
                            request_id = %request_id,
                            error = %e,
                            tier = ?tier_enum,
                            "Metrics recording failed. Observability degraded but stream continues."
                        );
                    }

                    // Mark endpoint as healthy
                    if let Err(e) = selector.health_checker().mark_success(&endpoint_name).await {
                        tracing::warn!(
                            request_id = %request_id,
                            endpoint_name = %endpoint_name,
                            error = %e,
                            "Health tracking failed for successful streaming completion"
                        );
                        // Record in metrics for observability parity with non-streaming handler
                        metrics.health_tracking_failure(&endpoint_name, e.error_type());
                    } else {
                        tracing::debug!(
                            request_id = %request_id,
                            endpoint_name = %endpoint_name,
                            "Marked endpoint healthy after successful stream completion"
                        );
                    }
                }
                // Return None to not emit any event - this is just for side effects
                None::<Result<Event, Infallible>>
            })
            .filter_map(|x| async { x })
        };

        // Combine: initial + content + finish + success tracking
        stream::iter(vec![initial_event])
            .chain(content_stream)
            .chain(finish_events)
            .chain(success_tracker)
            .boxed()
    })
    .flatten()
}
