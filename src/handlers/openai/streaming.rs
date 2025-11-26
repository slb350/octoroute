//! OpenAI-compatible streaming chat completions handler
//!
//! Handles POST /v1/chat/completions requests with stream: true.

use crate::config::Config;
use crate::error::AppError;
use crate::handlers::AppState;
use crate::middleware::RequestId;
use crate::router::TargetModel;
use crate::shared::query::record_routing_metrics;
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

use super::types::{ChatCompletionChunk, ChatCompletionRequest, ModelChoice};

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
                "Routing decision made (auto, streaming)"
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
                "Direct tier selection (streaming)"
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
                "Specific model selection (streaming)"
            );

            (decision, Some(endpoint.name().to_string()))
        }
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
            AppError::Internal(format!("Failed to configure model: {}", e))
        })?;

    // Generate unique ID and timestamp for this completion
    let completion_id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let response_model = model_name.unwrap_or_else(|| endpoint.name().to_string());

    tracing::info!(
        request_id = %request_id,
        completion_id = %completion_id,
        model = %response_model,
        endpoint_name = %endpoint.name(),
        "Starting streaming response"
    );

    // Create the SSE stream
    let stream = create_sse_stream(
        prompt,
        options,
        completion_id,
        response_model,
        created,
        request_id,
        endpoint.name().to_string(),
    );

    Ok(Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text(":\n\n"),
        )
        .into_response())
}

/// Create an SSE stream from the model query
fn create_sse_stream(
    prompt: String,
    options: open_agent::AgentOptions,
    completion_id: String,
    model: String,
    created: i64,
    request_id: RequestId,
    endpoint_name: String,
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
                // Return an error event
                let error_chunk = ChatCompletionChunk::content(
                    &completion_id,
                    &model,
                    created,
                    &format!("[Error: {}]", e),
                );
                return stream::iter(vec![
                    Ok(Event::default()
                        .data(serde_json::to_string(&error_chunk).unwrap_or_default())),
                    Ok(Event::default().data("[DONE]")),
                ])
                .boxed();
            }
        };

        // Create initial chunk with role announcement
        let initial = ChatCompletionChunk::initial(&completion_id, &model, created);
        let initial_event =
            Ok(Event::default().data(serde_json::to_string(&initial).unwrap_or_default()));

        // Map model stream to SSE events
        let content_stream = model_stream
            .filter_map({
                let completion_id = completion_id.clone();
                let model = model.clone();
                move |result| {
                    let completion_id = completion_id.clone();
                    let model = model.clone();
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
                                            serde_json::to_string(&chunk).unwrap_or_default(),
                                        )))
                                    }
                                    _ => None, // Skip non-text blocks
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Stream error during content delivery");
                                None
                            }
                        }
                    }
                }
            })
            .boxed();

        // Create finish chunk and done signal
        let finish_chunk = ChatCompletionChunk::finish(&completion_id, &model, created);
        let finish_events = stream::iter(vec![
            Ok(Event::default().data(serde_json::to_string(&finish_chunk).unwrap_or_default())),
            Ok(Event::default().data("[DONE]")),
        ]);

        // Combine: initial + content + finish
        stream::iter(vec![initial_event])
            .chain(content_stream)
            .chain(finish_events)
            .boxed()
    })
    .flatten()
}

/// Find an endpoint by name across all tiers
fn find_endpoint_by_name(
    config: &Config,
    name: &str,
) -> Result<(TargetModel, crate::config::ModelEndpoint), AppError> {
    // Search fast tier
    for endpoint in &config.models.fast {
        if endpoint.name() == name {
            return Ok((TargetModel::Fast, endpoint.clone()));
        }
    }

    // Search balanced tier
    for endpoint in &config.models.balanced {
        if endpoint.name() == name {
            return Ok((TargetModel::Balanced, endpoint.clone()));
        }
    }

    // Search deep tier
    for endpoint in &config.models.deep {
        if endpoint.name() == name {
            return Ok((TargetModel::Deep, endpoint.clone()));
        }
    }

    Err(AppError::Validation(format!(
        "Model '{}' not found. Available models: auto, fast, balanced, deep, or a specific endpoint name from config.",
        name
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config() -> Config {
        let toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "fast-model"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-model"
base_url = "http://localhost:1235/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-model"
base_url = "http://localhost:1236/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
"#;
        toml::from_str(toml).expect("should parse test config")
    }

    #[test]
    fn test_find_endpoint_by_name_fast() {
        let config = create_test_config();
        let (tier, endpoint) = find_endpoint_by_name(&config, "fast-model").unwrap();
        assert_eq!(tier, TargetModel::Fast);
        assert_eq!(endpoint.name(), "fast-model");
    }

    #[test]
    fn test_find_endpoint_by_name_not_found() {
        let config = create_test_config();
        let result = find_endpoint_by_name(&config, "nonexistent");
        assert!(result.is_err());
    }
}
