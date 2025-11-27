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
/// Maximum backoff duration in milliseconds (30 seconds)
///
/// Prevents infinite sleep from exponential overflow. With base=100ms:
/// - Attempt 9 would be 25.6 seconds (under cap)
/// - Attempt 10 would be 51.2 seconds (capped to 30s)
pub const MAX_BACKOFF_MS: u64 = 30_000;

/// Configuration for query execution with retries
#[derive(Debug, Clone)]
pub struct QueryConfig {
    /// Maximum number of retry attempts (must be at least 1)
    max_retries: usize,
    /// Base backoff in milliseconds (doubles each retry)
    retry_backoff_ms: u64,
}

/// Optional sampling parameters that override endpoint defaults
///
/// These parameters are passed through from OpenAI-compatible requests.
/// When `None`, the endpoint's configured defaults are used.
///
/// Note: The underlying open-agent-sdk only supports `temperature` and `max_tokens`.
/// Other OpenAI parameters (top_p, presence_penalty, frequency_penalty) are accepted
/// by the API for compatibility but are not forwarded to the backend.
#[derive(Debug, Clone, Default)]
pub struct SamplingParams {
    /// Override temperature (0.0 to 2.0)
    pub temperature: Option<f64>,
    /// Override max_tokens
    pub max_tokens: Option<u32>,
}

impl QueryConfig {
    /// Create a new query configuration
    ///
    /// # Errors
    /// Returns an error if `max_retries` is 0 (at least 1 attempt is required)
    pub fn new(max_retries: usize, retry_backoff_ms: u64) -> Result<Self, &'static str> {
        if max_retries == 0 {
            return Err("max_retries must be at least 1");
        }
        Ok(Self {
            max_retries,
            retry_backoff_ms,
        })
    }

    /// Get the maximum number of retry attempts
    pub fn max_retries(&self) -> usize {
        self.max_retries
    }

    /// Get the base backoff in milliseconds
    pub fn retry_backoff_ms(&self) -> u64 {
        self.retry_backoff_ms
    }
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_RETRIES, DEFAULT_RETRY_BACKOFF_MS)
            .expect("default QueryConfig values must be valid")
    }
}

/// Calculate exponential backoff with overflow protection
///
/// Returns the backoff duration in milliseconds for the given attempt number.
/// The formula is: `base * 2^(attempt-1)`, capped at [`MAX_BACKOFF_MS`].
///
/// # Arguments
/// * `config` - Query configuration containing base backoff
/// * `attempt` - Current attempt number (1-indexed)
///
/// # Returns
/// Backoff duration in milliseconds, never exceeding [`MAX_BACKOFF_MS`].
///
/// # Examples
/// With base=100ms:
/// - Attempt 1: 100ms
/// - Attempt 2: 200ms
/// - Attempt 3: 400ms
/// - Attempt 10+: 30,000ms (capped)
pub fn calculate_backoff(config: &QueryConfig, attempt: usize) -> u64 {
    let exponent = (attempt as u32).saturating_sub(1);
    config
        .retry_backoff_ms
        .saturating_mul(2_u64.saturating_pow(exponent))
        .min(MAX_BACKOFF_MS)
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
/// * `sampling_params` - Optional sampling parameters to override endpoint defaults
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
    sampling_params: Option<&SamplingParams>,
) -> AppResult<String> {
    // Determine effective sampling parameters (request overrides > endpoint defaults)
    let effective_max_tokens = sampling_params
        .and_then(|p| p.max_tokens)
        .unwrap_or(endpoint.max_tokens() as u32);
    let effective_temperature = sampling_params
        .and_then(|p| p.temperature)
        .map(|t| t as f32)
        .unwrap_or(endpoint.temperature() as f32);

    // Build AgentOptions with effective parameters
    let options = open_agent::AgentOptions::builder()
        .model(endpoint.name())
        .base_url(endpoint.base_url())
        .max_tokens(effective_max_tokens)
        .temperature(effective_temperature)
        .build()
        .map_err(|e| {
            tracing::error!(
                request_id = %request_id,
                endpoint_name = %endpoint.name(),
                endpoint_url = %endpoint.base_url(),
                max_tokens = effective_max_tokens,
                temperature = effective_temperature,
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
    sampling_params: Option<&SamplingParams>,
) -> AppResult<QueryResult> {
    let mut last_error = None;
    let mut failed_endpoints = ExclusionSet::new();
    let mut warnings: Vec<String> = Vec::new();

    // Add any warnings from the routing decision
    warnings.extend(decision.warnings().iter().cloned());

    for attempt in 1..=config.max_retries() {
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
                    max_retries = config.max_retries(),
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
                    config.max_retries()
                )));

                // Add exponential backoff before retry (capped to prevent overflow)
                if attempt < config.max_retries() {
                    let backoff_ms = calculate_backoff(config, attempt);
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
            max_retries = config.max_retries(),
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
            config.max_retries(),
            sampling_params,
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
                    max_retries = config.max_retries(),
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

                // Add exponential backoff before retry (capped to prevent overflow)
                if attempt < config.max_retries() {
                    let backoff_ms = calculate_backoff(config, attempt);
                    tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)).await;
                }
            }
        }
    }

    // All retries exhausted
    tracing::error!(
        request_id = %request_id,
        tier = ?decision.target(),
        max_retries = config.max_retries(),
        "All retry attempts exhausted"
    );

    Err(last_error.unwrap_or_else(|| {
        // Log full details for operators (including endpoint names)
        tracing::error!(
            request_id = %request_id,
            tier = ?decision.target(),
            max_retries = config.max_retries(),
            excluded_endpoints = ?failed_endpoints,
            "BUG: Retry loop exhausted but last_error is None"
        );

        // Return sanitized error to client (no internal endpoint names)
        // Include request_id so users can reference it when contacting support
        AppError::Internal(format!(
            "Request failed after {} retry attempts (request_id: {}). All {} endpoints for the {:?} tier \
            were exhausted. Please try again later.",
            config.max_retries(),
            request_id,
            failed_endpoints.len(),
            decision.target()
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
        assert_eq!(config.max_retries(), DEFAULT_MAX_RETRIES);
        assert_eq!(config.retry_backoff_ms(), DEFAULT_RETRY_BACKOFF_MS);
    }

    #[test]
    fn test_default_equivalent_to_new() {
        // Default should produce the same result as new() with default values
        let from_default = QueryConfig::default();
        let from_new = QueryConfig::new(DEFAULT_MAX_RETRIES, DEFAULT_RETRY_BACKOFF_MS)
            .expect("defaults valid");

        assert_eq!(from_default.max_retries(), from_new.max_retries());
        assert_eq!(from_default.retry_backoff_ms(), from_new.retry_backoff_ms());
    }

    #[test]
    fn test_query_config_new_valid() {
        let config = QueryConfig::new(5, 200).expect("should accept valid values");
        assert_eq!(config.max_retries(), 5);
        assert_eq!(config.retry_backoff_ms(), 200);
    }

    #[test]
    fn test_query_config_rejects_zero_retries() {
        let result = QueryConfig::new(0, 100);
        assert!(result.is_err(), "should reject zero retries");
        assert!(result.unwrap_err().contains("at least 1"));
    }

    #[test]
    fn test_query_config_accepts_one_retry() {
        let config = QueryConfig::new(1, 100).expect("should accept 1 retry");
        assert_eq!(config.max_retries(), 1);
    }

    // -------------------------------------------------------------------------
    // Backoff Calculation Tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_calculate_backoff_exponential_for_small_attempts() {
        let config = QueryConfig::new(5, 100).expect("valid config");

        // Attempt 1: base (100ms)
        assert_eq!(calculate_backoff(&config, 1), 100);
        // Attempt 2: base * 2 (200ms)
        assert_eq!(calculate_backoff(&config, 2), 200);
        // Attempt 3: base * 4 (400ms)
        assert_eq!(calculate_backoff(&config, 3), 400);
        // Attempt 4: base * 8 (800ms)
        assert_eq!(calculate_backoff(&config, 4), 800);
    }

    #[test]
    fn test_calculate_backoff_capped_at_maximum() {
        // Even with extreme values, backoff should be capped
        let config = QueryConfig::new(100, 100).expect("valid config");

        // Attempt 64 would overflow: 100 * 2^63 = way more than u64::MAX
        // Should be capped at MAX_BACKOFF_MS (30 seconds)
        let backoff = calculate_backoff(&config, 64);
        assert!(
            backoff <= MAX_BACKOFF_MS,
            "Backoff should be capped at {} ms, got {} ms",
            MAX_BACKOFF_MS,
            backoff
        );
        assert_eq!(backoff, MAX_BACKOFF_MS);
    }

    #[test]
    fn test_calculate_backoff_caps_before_overflow() {
        let config = QueryConfig::new(10, 1000).expect("valid config");

        // Attempt 10: 1000 * 2^9 = 512,000ms = 512 seconds
        // Should be capped at 30 seconds (30,000ms)
        let backoff = calculate_backoff(&config, 10);
        assert_eq!(backoff, MAX_BACKOFF_MS);
    }

    #[test]
    fn test_calculate_backoff_with_large_base() {
        // Large base that would exceed cap even on attempt 1
        let config = QueryConfig::new(3, 50_000).expect("valid config");

        // Attempt 1: 50,000ms > 30,000ms, should cap
        assert_eq!(calculate_backoff(&config, 1), MAX_BACKOFF_MS);
    }

    #[test]
    fn test_calculate_backoff_attempt_zero_treated_as_one() {
        // Edge case: attempt 0 should behave like attempt 1 (no negative exponent)
        let config = QueryConfig::new(3, 100).expect("valid config");

        // 2^(0-1) with saturating_sub = 2^0 = 1, so backoff = base
        let backoff = calculate_backoff(&config, 0);
        assert_eq!(backoff, 100);
    }
}
