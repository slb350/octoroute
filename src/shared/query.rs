//! Shared query execution logic with retry support
//!
//! This module provides reusable query execution that can be used by both
//! the legacy `/chat` endpoint and the OpenAI-compatible `/v1/chat/completions` endpoint.

use crate::config::ModelEndpoint;
use crate::error::{AppError, AppResult, ModelQueryError};
use crate::handlers::AppState;
use crate::middleware::RequestId;
use crate::models::{EndpointName, ExclusionSet};
use crate::router::{RoutingDecision, RoutingStrategy, TargetModel};
use std::time::Duration;

/// Default maximum number of retry attempts
pub const DEFAULT_MAX_RETRIES: usize = 3;
/// Default base backoff in milliseconds (doubles each retry)
pub const DEFAULT_RETRY_BACKOFF_MS: u64 = 100;

/// Configuration for query execution with retries
#[derive(Debug, Clone)]
pub struct QueryConfig {
    /// Maximum number of retry attempts
    pub max_retries: usize,
    /// Base backoff in milliseconds (doubles each retry)
    pub retry_backoff_ms: u64,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            retry_backoff_ms: DEFAULT_RETRY_BACKOFF_MS,
        }
    }
}

/// Result of a successful query execution
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// The response content from the model
    pub content: String,
    /// The endpoint that was used
    pub endpoint: ModelEndpoint,
    /// The tier that was used
    pub tier: TargetModel,
    /// The routing strategy that was used
    pub strategy: RoutingStrategy,
    /// Non-fatal warnings collected during execution
    pub warnings: Vec<String>,
}

/// Query a single endpoint with a prompt (no retry logic)
///
/// This is the core query function that sends a prompt to a specific endpoint
/// and returns the response. It handles timeout and error conversion but does
/// not retry on failure.
///
/// # Arguments
/// * `endpoint` - The model endpoint to query
/// * `prompt` - The prompt to send (can be a single message or combined messages)
/// * `timeout_seconds` - Maximum time to wait for response
/// * `request_id` - Request ID for logging
/// * `attempt` - Current attempt number (for logging)
/// * `max_retries` - Total number of retries (for logging)
///
/// # Returns
/// The response text on success, or an `AppError` on failure.
pub async fn query_model(
    endpoint: &ModelEndpoint,
    prompt: &str,
    timeout_seconds: u64,
    request_id: RequestId,
    attempt: usize,
    max_retries: usize,
) -> AppResult<String> {
    // Build AgentOptions from selected endpoint
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
                endpoint_url = %endpoint.base_url(),
                max_tokens = endpoint.max_tokens(),
                temperature = endpoint.temperature(),
                error = %e,
                "Failed to build AgentOptions from endpoint configuration"
            );
            AppError::ModelQuery(ModelQueryError::AgentOptionsConfigError {
                endpoint: endpoint.base_url().to_string(),
                details: format!("{}", e),
            })
        })?;

    tracing::debug!(
        request_id = %request_id,
        endpoint_name = %endpoint.name(),
        prompt_length = prompt.len(),
        timeout_seconds = timeout_seconds,
        "Starting model query"
    );

    let timeout_duration = Duration::from_secs(timeout_seconds);

    use futures::StreamExt;
    let timeout_result = tokio::time::timeout(timeout_duration, async {
        // Query model and get stream
        let mut stream = open_agent::query(prompt, &options).await.map_err(|e| {
            tracing::error!(
                request_id = %request_id,
                endpoint_name = %endpoint.name(),
                error = %e,
                "Failed to query model"
            );
            AppError::ModelQuery(ModelQueryError::StreamError {
                endpoint: endpoint.base_url().to_string(),
                bytes_received: 0,
                error_message: format!("{}", e),
            })
        })?;

        // Collect response from stream
        let mut response_text = String::new();
        let mut block_count = 0;
        while let Some(result) = stream.next().await {
            match result {
                Ok(block) => {
                    block_count += 1;
                    use open_agent::ContentBlock;
                    match block {
                        ContentBlock::Text(text_block) => {
                            response_text.push_str(&text_block.text);
                        }
                        other_block => {
                            tracing::warn!(
                                request_id = %request_id,
                                endpoint_name = %endpoint.name(),
                                block_type = ?other_block,
                                block_number = block_count,
                                "Received non-text content block, skipping (not supported - text blocks only)"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(
                        request_id = %request_id,
                        endpoint_name = %endpoint.name(),
                        endpoint_url = %endpoint.base_url(),
                        error = %e,
                        block_count = block_count,
                        partial_response_length = response_text.len(),
                        "Stream error after {} blocks ({} chars received). \
                        Discarding partial response and triggering retry.",
                        block_count,
                        response_text.len()
                    );
                    return Err(AppError::StreamInterrupted {
                        endpoint: endpoint.base_url().to_string(),
                        bytes_received: response_text.len(),
                        blocks_received: block_count,
                    });
                }
            }
        }

        Ok::<String, AppError>(response_text)
    })
    .await;

    // Handle timeout result
    let response_text = match timeout_result {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => return Err(e),
        Err(_elapsed) => {
            tracing::error!(
                request_id = %request_id,
                endpoint_name = %endpoint.name(),
                endpoint_url = %endpoint.base_url(),
                timeout_seconds = timeout_seconds,
                prompt_length = prompt.len(),
                attempt = attempt,
                max_retries = max_retries,
                "Request timed out. Endpoint: {} (attempt {}/{})",
                endpoint.base_url(),
                attempt,
                max_retries
            );
            return Err(AppError::EndpointTimeout {
                endpoint: endpoint.base_url().to_string(),
                timeout_seconds,
            });
        }
    };

    tracing::info!(
        endpoint_name = %endpoint.name(),
        response_length = response_text.len(),
        "Model query completed successfully"
    );

    Ok(response_text)
}

/// Execute a query with retry logic
///
/// This is the main entry point for executing a routed query with automatic
/// retry on failure. It handles:
/// - Endpoint selection from the target tier
/// - Request-scoped exclusion of failed endpoints
/// - Global health tracking
/// - Exponential backoff between retries
/// - Metrics recording
///
/// # Arguments
/// * `state` - Application state containing selector, health checker, metrics
/// * `decision` - Routing decision containing target tier and strategy
/// * `prompt` - The prompt to send to the model
/// * `request_id` - Request ID for logging and tracing
/// * `config` - Query configuration (retries, backoff)
///
/// # Returns
/// A `QueryResult` on success, containing the response and metadata.
/// An `AppError` if all retry attempts fail.
pub async fn execute_query_with_retry(
    state: &AppState,
    decision: &RoutingDecision,
    prompt: &str,
    request_id: RequestId,
    config: &QueryConfig,
) -> AppResult<QueryResult> {
    let mut last_error = None;
    let mut failed_endpoints = ExclusionSet::new();
    let mut warnings: Vec<String> = Vec::new();

    // Add any warnings from the routing decision
    warnings.extend(decision.warnings().iter().cloned());

    for attempt in 1..=config.max_retries {
        // Select endpoint from target tier (with health filtering + priority + exclusion)
        let endpoint = match state
            .selector()
            .select(decision.target(), &failed_endpoints)
            .await
        {
            Some(ep) => ep.clone(),
            None => {
                let total_configured = state.selector().endpoint_count(decision.target());
                let excluded_count = failed_endpoints.len();

                tracing::error!(
                    request_id = %request_id,
                    tier = ?decision.target(),
                    attempt = attempt,
                    max_retries = config.max_retries,
                    total_configured_endpoints = total_configured,
                    failed_endpoints_count = excluded_count,
                    failed_endpoints = ?failed_endpoints,
                    "No available healthy endpoints for tier. Configured: {}, Excluded: {}",
                    total_configured,
                    excluded_count
                );
                last_error = Some(AppError::RoutingFailed(format!(
                    "No available healthy endpoints for tier {:?} \
                    (configured: {}, excluded: {}, attempt {}/{})",
                    decision.target(),
                    total_configured,
                    excluded_count,
                    attempt,
                    config.max_retries
                )));

                // Add exponential backoff before retry
                if attempt < config.max_retries {
                    let backoff_ms = config
                        .retry_backoff_ms
                        .saturating_mul(2_u64.saturating_pow((attempt as u32).saturating_sub(1)));
                    tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                }
                continue;
            }
        };

        tracing::debug!(
            request_id = %request_id,
            endpoint_name = %endpoint.name(),
            endpoint_url = %endpoint.base_url(),
            attempt = attempt,
            max_retries = config.max_retries,
            "Attempting model query"
        );

        // Get timeout for this tier
        let timeout_seconds = state.config().timeout_for_tier(decision.target());

        // Try to query this endpoint
        match query_model(
            &endpoint,
            prompt,
            timeout_seconds,
            request_id,
            attempt,
            config.max_retries,
        )
        .await
        {
            Ok(response_text) => {
                // Success! Mark endpoint as healthy
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
                        "Health tracking skipped: {} (request continues with successful response)",
                        e
                    );
                    state
                        .metrics()
                        .health_tracking_failure(endpoint.name(), e.error_type());
                    warnings.push(format!(
                        "Health tracking failed: {} (endpoint health state may be stale)",
                        e
                    ));
                }

                tracing::info!(
                    request_id = %request_id,
                    endpoint_name = %endpoint.name(),
                    response_length = response_text.len(),
                    model_tier = ?decision.target(),
                    attempt = attempt,
                    "Query completed successfully"
                );

                // Record successful model invocation
                let tier_enum = match decision.target() {
                    TargetModel::Fast => crate::metrics::Tier::Fast,
                    TargetModel::Balanced => crate::metrics::Tier::Balanced,
                    TargetModel::Deep => crate::metrics::Tier::Deep,
                };

                if let Err(e) = state.metrics().record_model_invocation(tier_enum) {
                    state
                        .metrics()
                        .metrics_recording_failure("record_model_invocation");
                    tracing::error!(
                        request_id = %request_id,
                        error = %e,
                        tier = ?tier_enum,
                        "Metrics recording failed. Observability degraded but request continues."
                    );
                }

                return Ok(QueryResult {
                    content: response_text,
                    endpoint,
                    tier: decision.target(),
                    strategy: decision.strategy(),
                    warnings,
                });
            }
            Err(e) => {
                // Failure - mark for health tracking and exclude from retries
                tracing::warn!(
                    request_id = %request_id,
                    endpoint_name = %endpoint.name(),
                    attempt = attempt,
                    max_retries = config.max_retries,
                    error = %e,
                    "Endpoint query failed, excluding from retries"
                );

                // Mark endpoint as failed for global health tracking
                if let Err(health_err) = state
                    .selector()
                    .health_checker()
                    .mark_failure(endpoint.name())
                    .await
                {
                    tracing::warn!(
                        request_id = %request_id,
                        endpoint_name = %endpoint.name(),
                        error = %health_err,
                        "Health tracking skipped: {} (request continues with retry logic)",
                        health_err
                    );
                    state
                        .metrics()
                        .health_tracking_failure(endpoint.name(), health_err.error_type());
                    warnings.push(format!(
                        "Health tracking failed: {} (endpoint health state may be stale)",
                        health_err
                    ));
                }

                // Exclude from this request's retries
                failed_endpoints.insert(EndpointName::from(&endpoint));
                last_error = Some(e);

                // Add exponential backoff before retry
                if attempt < config.max_retries {
                    let backoff_ms = config
                        .retry_backoff_ms
                        .saturating_mul(2_u64.saturating_pow((attempt as u32).saturating_sub(1)));
                    tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }

    // All retries exhausted
    tracing::error!(
        request_id = %request_id,
        tier = ?decision.target(),
        max_retries = config.max_retries,
        "All retry attempts exhausted"
    );

    Err(last_error.unwrap_or_else(|| {
        tracing::error!(
            request_id = %request_id,
            tier = ?decision.target(),
            max_retries = config.max_retries,
            excluded_endpoints = ?failed_endpoints,
            "BUG: Retry loop exhausted but last_error is None"
        );

        AppError::Internal(format!(
            "Request failed after {} retry attempts. All endpoints for tier {:?} \
            were exhausted. Failed endpoints: {:?}.",
            config.max_retries,
            decision.target(),
            failed_endpoints
                .iter()
                .map(|ep| ep.as_str())
                .collect::<Vec<_>>()
        ))
    }))
}

/// Record routing metrics
///
/// Records the routing decision metrics (tier, strategy, duration).
/// Failures are logged but do not cause the request to fail.
pub fn record_routing_metrics(
    state: &AppState,
    decision: &RoutingDecision,
    routing_duration_ms: f64,
    request_id: RequestId,
) {
    let metrics = state.metrics();

    let tier_enum = match decision.target() {
        TargetModel::Fast => crate::metrics::Tier::Fast,
        TargetModel::Balanced => crate::metrics::Tier::Balanced,
        TargetModel::Deep => crate::metrics::Tier::Deep,
    };
    let strategy_enum = match decision.strategy() {
        RoutingStrategy::Rule => crate::metrics::Strategy::Rule,
        RoutingStrategy::Llm => crate::metrics::Strategy::Llm,
    };

    if let Err(e) = metrics.record_request(tier_enum, strategy_enum) {
        metrics.metrics_recording_failure("record_request");
        tracing::error!(
            request_id = %request_id,
            error = %e,
            tier = ?tier_enum,
            strategy = ?strategy_enum,
            "Metrics recording failed. Observability degraded but request continues."
        );
    }

    if let Err(e) = metrics.record_routing_duration(strategy_enum, routing_duration_ms) {
        metrics.metrics_recording_failure("record_routing_duration");
        tracing::error!(
            request_id = %request_id,
            error = %e,
            strategy = ?strategy_enum,
            duration_ms = routing_duration_ms,
            "Metrics recording failed. Observability degraded but request continues."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_config_default() {
        let config = QueryConfig::default();
        assert_eq!(config.max_retries, DEFAULT_MAX_RETRIES);
        assert_eq!(config.retry_backoff_ms, DEFAULT_RETRY_BACKOFF_MS);
    }

    #[test]
    fn test_query_config_custom() {
        let config = QueryConfig {
            max_retries: 5,
            retry_backoff_ms: 200,
        };
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.retry_backoff_ms, 200);
    }
}
