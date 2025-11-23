//! LLM-based router that uses an LLM to make intelligent routing decisions
//!
//! Uses a configurable tier (Fast/Balanced/Deep) via config.routing.router_tier to analyze
//! requests and choose the optimal target model. This is a pure LLM routing strategy that
//! always uses LLM analysis (not a fallback - see HybridRouter for rule+LLM fallback).
//!
//! ## Tier Selection for Routing
//!
//! See [`TierSelector`] documentation for tier comparison,
//! latency characteristics, and trade-offs when choosing a router tier.

use crate::error::{AppError, AppResult};
use crate::models::endpoint_name::ExclusionSet;
use crate::models::{ModelSelector, TierSelector};
use crate::router::{RouteMetadata, RoutingDecision, RoutingStrategy, TargetModel};
use async_trait::async_trait;
use std::sync::Arc;

/// Trait for LLM-based routing
///
/// Allows dependency injection of different LLM router implementations,
/// enabling testing with mock routers that don't make real network calls.
#[async_trait]
pub trait LlmRouter: Send + Sync {
    /// Route a request based on LLM analysis
    ///
    /// # Arguments
    ///
    /// * `user_prompt` - The user's original prompt
    /// * `meta` - Request metadata (token estimate, importance, task type)
    ///
    /// # Returns
    ///
    /// Returns a routing decision indicating which tier to use, or an error
    /// if routing fails (no healthy endpoints, LLM malfunction, etc.)
    async fn route(&self, user_prompt: &str, meta: &RouteMetadata) -> AppResult<RoutingDecision>;
}

/// Errors specific to LLM-based routing decisions
///
/// Categorizes errors into systemic (LLM malfunction) vs transient (network/endpoint issues).
/// This allows the retry logic to distinguish between errors that should fail fast vs errors
/// that can be resolved by trying a different endpoint.
#[derive(Debug, thiserror::Error)]
pub enum LlmRouterError {
    /// Router LLM returned empty response (no content blocks)
    ///
    /// Systemic error - indicates LLM malfunction or misconfiguration.
    /// Retrying with a different endpoint won't help.
    #[error(
        "Router LLM returned empty response from {endpoint}. \
         Expected response containing one of: FAST, BALANCED, or DEEP. \
         Possible causes: safety filter activation, API failure, streaming error, or LLM misconfiguration."
    )]
    EmptyResponse { endpoint: String },

    /// Router LLM returned unparseable response (no valid routing keyword found)
    ///
    /// Systemic error - indicates LLM not following instructions, safety filter activation,
    /// or misconfiguration. Retrying won't help.
    ///
    /// The `response` field contains a truncated preview (max 500 chars).
    /// The `response_length` field contains the total original response length.
    #[error("Router LLM returned unparseable response ({response_length} bytes): {response}")]
    UnparseableResponse {
        endpoint: String,
        response: String,
        response_length: usize,
    },

    /// Router LLM returned refusal or error message
    ///
    /// Systemic error - indicates safety filter activation or LLM refusing the request.
    /// Retrying won't help.
    #[error("Router LLM refused or returned error: {message}")]
    Refusal { endpoint: String, message: String },

    /// Router response exceeded size limit ({size} bytes > {max_size} bytes)
    ///
    /// Systemic error - indicates LLM generating essays instead of classifications,
    /// infinite generation loops, or prompt injection. Retrying won't help.
    #[error(
        "Router response exceeded {max_size} bytes (got {size} bytes). LLM not following instructions."
    )]
    SizeExceeded {
        endpoint: String,
        size: usize,
        max_size: usize,
    },

    /// Failed to configure AgentOptions for router query
    ///
    /// Systemic error - indicates configuration problem (invalid model name, base_url, etc.).
    /// Retrying won't help.
    #[error("Failed to configure AgentOptions for router: {details}")]
    AgentOptionsConfigError { endpoint: String, details: String },

    /// Stream error while receiving router response
    ///
    /// Transient error - network interruption, timeout, or connection loss mid-stream.
    /// Retrying with a different endpoint may succeed.
    #[error("Stream error after {bytes_received} bytes received: {error_message}")]
    StreamError {
        endpoint: String,
        bytes_received: usize,
        error_message: String,
    },

    /// Query timeout waiting for router response
    ///
    /// Transient error - endpoint may be overloaded or unreachable.
    /// Retrying with a different endpoint may succeed.
    #[error(
        "Router query timed out after {timeout_seconds}s (attempt {attempt}/{max_attempts}) for {router_tier:?} tier. \
         Remediation: Check endpoint health at {endpoint}, increase timeout in config, or try a faster tier."
    )]
    Timeout {
        endpoint: String,
        timeout_seconds: u64,
        attempt: usize,
        max_attempts: usize,
        router_tier: crate::router::TargetModel,
    },
}

impl LlmRouterError {
    /// Returns true if this error is retryable (transient network/endpoint issue)
    ///
    /// Retryable errors:
    /// - StreamError: Network interruption, may succeed with different endpoint
    /// - Timeout: Endpoint overloaded, may succeed with different endpoint
    ///
    /// Non-retryable (systemic) errors:
    /// - EmptyResponse: LLM malfunction
    /// - UnparseableResponse: LLM not following instructions
    /// - Refusal: Safety filter or LLM refusing request
    /// - SizeExceeded: LLM generating invalid output
    /// - AgentOptionsConfigError: Configuration problem
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            LlmRouterError::StreamError { .. } | LlmRouterError::Timeout { .. }
        )
    }
}

// From<LlmRouterError> for AppError is auto-generated by the #[from] attribute
// on AppError::LlmRouting variant. This preserves type information instead of
// losing it by converting to AppError::ModelQueryFailed.

/// Maximum size for router LLM response (bytes)
///
/// This 1KB limit is a **defensive safeguard** to catch runaway LLM generation early
/// and prevent unbounded memory growth. Responses exceeding this limit are rejected
/// during streaming without parsing (error returned immediately).
///
/// ## Expected Response Format
///
/// Router responses contain one of three keywords: "FAST", "BALANCED", or "DEEP".
/// The parser extracts these keywords from verbose responses (e.g., "I recommend
/// BALANCED because it provides the best balance of speed and accuracy for this task").
///
/// **Typical response sizes**:
/// - Minimal: "BALANCED" (~10 bytes)
/// - Verbose: "I recommend BALANCED because..." (~100-200 bytes)
/// - Excessive: Multi-paragraph explanations (>500 bytes, likely indicates issues)
///
/// ## Rationale for 1KB Limit
///
/// The 1KB limit accommodates legitimate verbose responses (100-200 bytes) while
/// preventing unbounded memory growth from:
/// - Runaway generation (repeating text, hallucination loops)
/// - LLM generating essays instead of classifications (>100 words)
/// - Prompt injection attempts attempting to overwhelm the parser
///
/// This is 5-10x larger than typical verbose responses, providing a comfortable safety
/// margin while still catching problematic generation early.
const MAX_ROUTER_RESPONSE: usize = 1024;

/// LLM-powered router that uses a model to make routing decisions
///
/// Uses the configured tier to analyze requests and choose optimal target.
/// Provides intelligent fallback when rule-based routing is ambiguous.
///
/// # Construction-Time Validation
///
/// Uses `TierSelector` to validate that the specified tier has available endpoints.
/// The tier is chosen via `config.routing.router_tier` at construction time.
pub struct LlmBasedRouter {
    selector: TierSelector,
    router_tier: TargetModel,
    metrics: Arc<crate::metrics::Metrics>,
}

impl LlmBasedRouter {
    /// Create a new LLM-based router using the specified tier
    ///
    /// Returns an error if no endpoints are configured for the specified tier.
    ///
    /// # Arguments
    /// * `selector` - The underlying ModelSelector
    /// * `tier` - Which tier (Fast, Balanced, Deep) to use for routing decisions
    /// * `metrics` - Metrics collector for observability
    ///
    /// # Tier Selection
    ///
    /// - **Fast**: Lowest latency (~50-200ms) but may misroute complex requests
    /// - **Balanced**: Recommended default (~100-500ms) with good accuracy
    /// - **Deep**: Highest accuracy (~2-5s) but rarely worth the latency overhead
    ///
    /// # Construction-Time Validation
    ///
    /// The `TierSelector` validates tier availability at construction, ensuring
    /// at least one endpoint exists for the specified tier.
    pub fn new(
        selector: Arc<ModelSelector>,
        tier: TargetModel,
        metrics: Arc<crate::metrics::Metrics>,
    ) -> AppResult<Self> {
        // TierSelector validates that the tier exists
        let tier_selector = TierSelector::new(selector, tier)?;

        Ok(Self {
            selector: tier_selector,
            router_tier: tier,
            metrics,
        })
    }

    /// Get the tier this router uses for routing decisions
    ///
    /// Returns the tier configured via `router_tier` in config.toml.
    /// This tier determines which model endpoints are queried when making
    /// routing decisions.
    pub fn tier(&self) -> TargetModel {
        self.router_tier
    }

    /// Classify error as retryable (transient) or non-retryable (systemic)
    ///
    /// Systemic errors indicate problems that won't be fixed by retrying with
    /// a different endpoint:
    /// - Parse failures (unparseable, empty, or oversized responses)
    /// - Refusal/error responses from LLM
    /// - Configuration errors
    ///
    /// Transient errors may be resolved by retrying with a different endpoint:
    /// - Network timeouts
    /// - Connection failures
    /// - Stream errors
    ///
    /// # Implementation
    ///
    /// Uses type-safe error classification via LlmRouterError::is_retryable()
    /// and ModelQueryError::is_retryable().
    fn is_retryable_error(error: &AppError) -> bool {
        match error {
            // Type-safe error classification - no string matching!
            AppError::LlmRouting(e) => e.is_retryable(),
            AppError::ModelQuery(e) => e.is_retryable(),

            // Config errors are systemic, never retryable
            AppError::Config(_)
            | AppError::ConfigFileRead { .. }
            | AppError::ConfigParseFailed { .. }
            | AppError::ConfigValidationFailed { .. } => false,

            // Default: assume transient for unknown error types
            // Conservative approach - retry unless we know it's systemic
            _ => true,
        }
    }

    /// Route request using LLM analysis
    ///
    /// # Async Behavior
    /// This method is async because it:
    /// - **Waits for LLM inference**: ~100-500ms for 30B model routing decision (dominant latency)
    /// - Makes HTTP requests to LLM endpoints (network I/O, ~10-100ms connection overhead)
    /// - Awaits endpoint selection from ModelSelector (async lock acquisition, <1ms)
    /// - Performs health tracking mark_success/mark_failure (async lock, <1ms)
    ///
    /// Total typical latency: ~110-600ms (dominated by LLM inference)
    ///
    /// # Retry Logic & Failure Tracking (Dual-Level)
    /// Implements sophisticated retry with TWO failure tracking mechanisms:
    /// 1. **Request-Scoped Exclusion** (`failed_endpoints`): Prevents retrying
    ///    the same endpoint within THIS request. Clears when function returns.
    /// 2. **Global Health Tracking**: Marks endpoints unhealthy after 3 consecutive
    ///    failures across ALL requests. Persists via ModelSelector's health_checker.
    ///
    /// # Cancellation Safety
    /// If the returned Future is dropped (cancelled), in-flight LLM queries will be
    /// aborted but endpoint health state remains consistent (mark_success/mark_failure
    /// only called after query completes).
    pub async fn route(
        &self,
        user_prompt: &str,
        meta: &RouteMetadata,
    ) -> AppResult<RoutingDecision> {
        // Build router prompt
        let router_prompt = Self::build_router_prompt(user_prompt, meta);

        tracing::debug!(
            prompt_length = router_prompt.len(),
            user_prompt_length = user_prompt.len(),
            "Built router prompt for LLM analysis"
        );

        // Retry loop with request-scoped exclusion (similar to chat handler)
        //
        // SCOPE: The `failed_endpoints` exclusion set is request-scoped - it exists only
        // for the duration of this function call and is discarded when the function returns.
        // This means endpoints excluded during this request's retries will be available again
        // for the next request.
        //
        // WHY REQUEST-SCOPED: If we permanently excluded endpoints after failures, a single
        // transient network glitch could permanently remove a healthy endpoint from rotation.
        // Request-scoped exclusions allow the health checker to independently track endpoint
        // health and recover failed endpoints, while still preventing retry loops from
        // hitting the same failed endpoint repeatedly within a single request.
        const MAX_ROUTER_RETRIES: usize = 2;
        let mut last_error = None;
        let mut failed_endpoints = ExclusionSet::new();
        let mut warnings = Vec::new(); // Collect health tracking failures to surface to user

        for attempt in 1..=MAX_ROUTER_RETRIES {
            // Select endpoint from router tier (with health filtering + exclusions)
            let endpoint = match self.selector.select(&failed_endpoints).await {
                Some(ep) => ep.clone(),
                None => {
                    let total_configured = self.selector.endpoint_count();
                    let excluded_count = failed_endpoints.len();
                    let router_tier = self.selector.tier();

                    // Categorize failure type for better diagnostics and actionable guidance
                    if total_configured == 0 {
                        // CONFIGURATION ERROR: No endpoints configured for this tier
                        // This should have been caught by Config::validate() but check defensively
                        tracing::error!(
                            tier = ?router_tier,
                            attempt = attempt,
                            max_retries = MAX_ROUTER_RETRIES,
                            "CONFIGURATION ERROR: No endpoints configured for {:?} tier. \
                            Check config.toml: [[models.{:?}]] section must have at least one endpoint. \
                            This should have been caught by validation.",
                            router_tier, router_tier
                        );
                        last_error = Some(AppError::Config(format!(
                            "No endpoints configured for {:?} tier (router_tier setting). \
                            Add at least one endpoint to [[models.{:?}]] in config.toml.",
                            router_tier, router_tier
                        )));
                        continue;
                    } else if excluded_count == total_configured {
                        // COMPLETE EXHAUSTION: All configured endpoints tried and failed
                        // This is an error condition - we tried everything and nothing worked

                        // Collect failed endpoint names for debugging
                        let failed_names: Vec<&str> =
                            failed_endpoints.iter().map(|ep| ep.as_str()).collect();
                        let failed_names_str = failed_names.join(", ");

                        tracing::error!(
                            tier = ?router_tier,
                            attempt = attempt,
                            max_retries = MAX_ROUTER_RETRIES,
                            total_configured_endpoints = total_configured,
                            failed_endpoints = ?failed_endpoints,
                            "COMPLETE EXHAUSTION: All {} {:?} tier endpoints failed for routing. \
                            All endpoints tried in this request returned errors. Check endpoint health.",
                            total_configured, router_tier
                        );
                        last_error = Some(AppError::RoutingFailed(format!(
                            "All {} {:?} tier endpoints exhausted for routing (attempt {}/{}). \
                            Failed endpoints: {}. \
                            Check endpoint connectivity and health.",
                            total_configured,
                            router_tier,
                            attempt,
                            MAX_ROUTER_RETRIES,
                            failed_names_str
                        )));
                        continue;
                    } else {
                        // TRANSIENT FAILURE: Some endpoints exist but are unhealthy, waiting for recovery
                        // This is a warning, not an error - endpoints may recover soon
                        let healthy_count = total_configured - excluded_count;
                        tracing::warn!(
                            tier = ?router_tier,
                            attempt = attempt,
                            max_retries = MAX_ROUTER_RETRIES,
                            total_configured_endpoints = total_configured,
                            failed_endpoints_count = excluded_count,
                            healthy_but_unavailable_count = healthy_count,
                            failed_endpoints = ?failed_endpoints,
                            "TRANSIENT: No available {:?} tier endpoints (configured: {}, failed: {}, \
                            healthy but unavailable: {}). Endpoints may be recovering from failures. \
                            Waiting for health checker recovery.",
                            router_tier, total_configured, excluded_count, healthy_count
                        );
                        last_error = Some(AppError::RoutingFailed(format!(
                            "No available {:?} tier endpoints (configured: {}, failed: {}, \
                            healthy but temporarily unavailable: {}, attempt {}/{}). \
                            Endpoints may recover shortly.",
                            router_tier,
                            total_configured,
                            excluded_count,
                            healthy_count,
                            attempt,
                            MAX_ROUTER_RETRIES
                        )));
                        continue;
                    }
                }
            };

            tracing::debug!(
                endpoint_name = %endpoint.name(),
                endpoint_url = %endpoint.base_url(),
                tier = ?self.selector.tier(),
                attempt = attempt,
                max_retries = MAX_ROUTER_RETRIES,
                "Selected {:?} tier endpoint for routing decision",
                self.selector.tier()
            );

            // Try to query this endpoint
            let query_result = self
                .try_router_query(&endpoint, &router_prompt, attempt, MAX_ROUTER_RETRIES)
                .await;

            match query_result {
                Ok(target_model) => {
                    // Success! Mark endpoint healthy for immediate recovery
                    //
                    // DEFENSIVE: mark_success should never fail in normal operation because endpoint
                    // names come from ModelSelector which only returns valid endpoints. However, we check
                    // explicitly to catch rare edge cases (race conditions, config reload mid-request, or bugs).
                    //
                    // IMPORTANT: If health tracking fails, we LOG the error but DO NOT fail the request.
                    // Health tracking is auxiliary functionality - the routing decision succeeded and that's
                    // what matters. Failing the request would discard a valid routing decision.
                    //
                    // OPERATOR IMPACT: When health tracking fails, the system experiences degraded observability:
                    //
                    // - **Slower Recovery**: Failed endpoints won't be marked healthy immediately. The health
                    //   checker's background polling will eventually recover them, but recovery time increases
                    //   from immediate (on success) to the next health check interval (typically 30-60 seconds).
                    //
                    // - **Suboptimal Routing**: During the recovery delay, the router may avoid a perfectly
                    //   healthy endpoint because health tracking couldn't update its status. This can lead to
                    //   higher latency if the router uses slower endpoints or retries more frequently.
                    //
                    // - **Warning Surfaced**: The failure is surfaced to users as a warning in the response,
                    //   alerting operators to investigate potential configuration issues, race conditions, or bugs.
                    //
                    // The request still succeeds, but operators should monitor health_tracking_failure metrics
                    // to detect and fix underlying issues.
                    if let Err(e) = self
                        .selector
                        .health_checker()
                        .mark_success(endpoint.name())
                        .await
                    {
                        use crate::models::health::HealthError;
                        let warning_msg = match e {
                            HealthError::UnknownEndpoint(ref name) => {
                                self.metrics.health_tracking_failure();
                                tracing::error!(
                                    endpoint_name = %endpoint.name(),
                                    unknown_name = %name,
                                    target_model = ?target_model,
                                    attempt = attempt,
                                    "DEFENSIVE: mark_success failed with unknown endpoint. \
                                    Endpoint names come from ModelSelector which only returns valid endpoints. \
                                    This indicates a serious bug (race condition, naming mismatch, or config \
                                    reload during request). Continuing with routing decision but endpoint \
                                    won't be marked healthy (health tracking degraded)."
                                );
                                format!(
                                    "Health tracking failure: UnknownEndpoint '{}'. Endpoint recovery may be impaired.",
                                    name
                                )
                            }
                            HealthError::HttpClientCreationFailed(ref msg) => {
                                self.metrics.health_tracking_failure();
                                tracing::error!(
                                    endpoint_name = %endpoint.name(),
                                    error = %msg,
                                    target_model = ?target_model,
                                    attempt = attempt,
                                    "DEFENSIVE: HTTP client creation failed during health tracking. \
                                    This indicates a systemic issue (TLS configuration, resource exhaustion). \
                                    Continuing with routing decision but health tracking is degraded."
                                );
                                format!(
                                    "Health tracking failure: HTTP client creation failed ({}). Health monitoring degraded.",
                                    msg
                                )
                            }
                        };
                        warnings.push(warning_msg);
                        // DO NOT return error - we have a valid routing decision
                    }

                    tracing::info!(
                        endpoint_name = %endpoint.name(),
                        target_model = ?target_model,
                        attempt = attempt,
                        warnings_count = warnings.len(),
                        "Router LLM successfully determined target model"
                    );

                    // Return routing decision with any collected warnings
                    let mut decision = RoutingDecision::new(target_model, RoutingStrategy::Llm);
                    for warning in warnings {
                        decision = decision.with_warning(warning);
                    }
                    return Ok(decision);
                }
                Err(e) => {
                    // Classify error as retryable or systemic
                    let is_retryable = Self::is_retryable_error(&e);

                    if !is_retryable {
                        // Systemic error - fail fast without retrying
                        // Examples: parse failures, config errors, unparseable responses
                        tracing::error!(
                            endpoint_name = %endpoint.name(),
                            attempt = attempt,
                            error = %e,
                            "Router query failed with systemic error - failing fast (no retry)"
                        );
                        return Err(e);
                    }

                    // Transient error - mark endpoint unhealthy and retry with different endpoint
                    tracing::warn!(
                        endpoint_name = %endpoint.name(),
                        attempt = attempt,
                        max_retries = MAX_ROUTER_RETRIES,
                        error = %e,
                        "Router query failed with transient error, marking endpoint and retrying"
                    );

                    // DEFENSIVE: mark_failure should never fail in normal operation because endpoint
                    // names come from ModelSelector which only returns valid endpoints. However, we check
                    // explicitly to catch rare edge cases (race conditions, config reload mid-request, or bugs).
                    //
                    // IMPORTANT: If health tracking fails, we LOG the error but DO NOT fail the request.
                    // The endpoint won't be marked unhealthy (health tracking degraded), but we continue
                    // with retry logic using the exclusion set (which still prevents immediate retry).
                    if let Err(health_err) = self
                        .selector
                        .health_checker()
                        .mark_failure(endpoint.name())
                        .await
                    {
                        use crate::models::health::HealthError;
                        let warning_msg = match health_err {
                            HealthError::UnknownEndpoint(ref name) => {
                                self.metrics.health_tracking_failure();
                                tracing::error!(
                                    endpoint_name = %endpoint.name(),
                                    unknown_name = %name,
                                    attempt = attempt,
                                    "DEFENSIVE: mark_failure failed with unknown endpoint. \
                                    Endpoint won't be marked unhealthy globally but will still be excluded \
                                    from THIS request via exclusion set. Endpoint names come from ModelSelector \
                                    which only returns valid endpoints. This indicates a serious bug (race \
                                    condition or naming mismatch). Continuing with retry (health tracking degraded)."
                                );
                                format!(
                                    "Health tracking failure: UnknownEndpoint '{}' during failure marking. Endpoint recovery may be impaired.",
                                    name
                                )
                            }
                            HealthError::HttpClientCreationFailed(ref msg) => {
                                self.metrics.health_tracking_failure();
                                tracing::error!(
                                    endpoint_name = %endpoint.name(),
                                    error = %msg,
                                    attempt = attempt,
                                    "DEFENSIVE: HTTP client creation failed during health tracking. \
                                    This indicates a systemic issue (TLS configuration, resource exhaustion). \
                                    Continuing with retry but health tracking is degraded."
                                );
                                format!(
                                    "Health tracking failure: HTTP client creation failed ({}) during failure marking. Health monitoring degraded.",
                                    msg
                                )
                            }
                        };
                        warnings.push(warning_msg);
                        // DO NOT return error - continue with retry logic
                    }

                    // Add to exclusion set to prevent retry on same endpoint
                    use crate::models::EndpointName;
                    failed_endpoints.insert(EndpointName::from(&endpoint));
                    last_error = Some(e);
                    continue; // Try next endpoint
                }
            }
        }

        // All retries exhausted
        tracing::error!(
            tier = ?self.selector.tier(),
            max_retries = MAX_ROUTER_RETRIES,
            "All router retry attempts exhausted"
        );

        Err(last_error.unwrap_or_else(|| {
            AppError::RoutingFailed(format!(
                "All {} router retry attempts exhausted",
                MAX_ROUTER_RETRIES
            ))
        }))
    }

    /// Helper to attempt a single router query (extracted for retry logic)
    async fn try_router_query(
        &self,
        endpoint: &crate::config::ModelEndpoint,
        router_prompt: &str,
        attempt: usize,
        max_retries: usize,
    ) -> AppResult<TargetModel> {
        // Build AgentOptions from endpoint
        let options = open_agent::AgentOptions::builder()
            .model(endpoint.name())
            .base_url(endpoint.base_url())
            .max_tokens(endpoint.max_tokens() as u32)
            .temperature(endpoint.temperature() as f32)
            .build()
            .map_err(|e| {
                tracing::error!(
                    endpoint_name = %endpoint.name(),
                    endpoint_url = %endpoint.base_url(),
                    model = %endpoint.name(),
                    max_tokens = endpoint.max_tokens(),
                    temperature = endpoint.temperature(),
                    error = %e,
                    attempt = attempt,
                    max_retries = max_retries,
                    "Failed to build AgentOptions for router query"
                );
                AppError::LlmRouting(LlmRouterError::AgentOptionsConfigError {
                    endpoint: endpoint.base_url().to_string(),
                    details: format!(
                        "{}. Check configuration: model='{}' (must be non-empty), \
                         max_tokens={} (must be > 0), base_url='{}' (must end with /v1)",
                        e,
                        endpoint.name(),
                        endpoint.max_tokens(),
                        endpoint.base_url()
                    ),
                })
            })?;

        // Query the router model with timeout protection
        use futures::StreamExt;
        use tokio::time::{Duration, timeout};

        const ROUTER_QUERY_TIMEOUT_SECS: u64 = 10;
        let timeout_duration = Duration::from_secs(ROUTER_QUERY_TIMEOUT_SECS);

        let mut stream = timeout(timeout_duration, open_agent::query(router_prompt, &options))
            .await
            .map_err(|_elapsed| {
                tracing::error!(
                    endpoint_name = %endpoint.name(),
                    endpoint_url = %endpoint.base_url(),
                    timeout_seconds = ROUTER_QUERY_TIMEOUT_SECS,
                    router_tier = ?self.router_tier,
                    attempt = attempt,
                    max_retries = max_retries,
                    "Router query timeout - endpoint did not respond within {} seconds (attempt {}/{})",
                    ROUTER_QUERY_TIMEOUT_SECS, attempt, max_retries
                );
                AppError::LlmRouting(LlmRouterError::Timeout {
                    endpoint: endpoint.base_url().to_string(),
                    timeout_seconds: ROUTER_QUERY_TIMEOUT_SECS,
                    attempt,
                    max_attempts: max_retries,
                    router_tier: self.router_tier,
                })
            })?
            .map_err(|e| {
                tracing::error!(
                    endpoint_name = %endpoint.name(),
                    endpoint_url = %endpoint.base_url(),
                    error = %e,
                    attempt = attempt,
                    max_retries = max_retries,
                    "Router query failed to connect or initialize stream (attempt {}/{})",
                    attempt, max_retries
                );
                AppError::LlmRouting(LlmRouterError::StreamError {
                    endpoint: endpoint.base_url().to_string(),
                    bytes_received: 0,
                    error_message: format!("Router query failed: {}", e),
                })
            })?;

        // Collect response from stream with size limit to prevent unbounded memory growth
        let mut response_text = String::new();
        while let Some(result) = stream.next().await {
            match result {
                Ok(block) => {
                    use open_agent::ContentBlock;
                    if let ContentBlock::Text(text_block) = block {
                        // Check size limit before accumulating
                        if response_text.len() + text_block.text.len() > MAX_ROUTER_RESPONSE {
                            // Oversized response indicates serious LLM malfunction
                            // Expected response: ~10 bytes ("FAST", "BALANCED", or "DEEP")
                            // >1KB response means LLM is ignoring instructions or misconfigured

                            // Capture preview of response for debugging (first 200 chars)
                            let preview_chars: String = response_text.chars().take(200).collect();
                            let preview = if response_text.len() > 200 {
                                format!("{}...", preview_chars)
                            } else {
                                preview_chars
                            };

                            tracing::error!(
                                endpoint_name = %endpoint.name(),
                                current_length = response_text.len(),
                                incoming_length = text_block.text.len(),
                                max_allowed = MAX_ROUTER_RESPONSE,
                                response_preview = %preview,
                                attempt = attempt,
                                max_retries = max_retries,
                                "Router response exceeded size limit - LLM not following instructions (attempt {}/{})",
                                attempt, max_retries
                            );
                            return Err(AppError::LlmRouting(LlmRouterError::SizeExceeded {
                                endpoint: endpoint.base_url().to_string(),
                                size: response_text.len() + text_block.text.len(),
                                max_size: MAX_ROUTER_RESPONSE,
                            }));
                        }
                        response_text.push_str(&text_block.text);
                    }
                }
                Err(e) => {
                    tracing::error!(
                        endpoint_name = %endpoint.name(),
                        endpoint_url = %endpoint.base_url(),
                        error = %e,
                        partial_response_length = response_text.len(),
                        attempt = attempt,
                        max_retries = max_retries,
                        "Router stream error after {} chars (attempt {}/{})",
                        response_text.len(), attempt, max_retries
                    );
                    return Err(AppError::LlmRouting(LlmRouterError::StreamError {
                        endpoint: endpoint.base_url().to_string(),
                        bytes_received: response_text.len(),
                        error_message: format!("{}", e),
                    }));
                }
            }
        }

        // Early empty response detection: fail immediately after streaming completes
        // if no content received, instead of waiting for parse_routing_decision() to detect it.
        // This optimization saves processing time on obvious LLM malfunctions.
        if response_text.trim().is_empty() {
            tracing::error!(
                endpoint_name = %endpoint.name(),
                endpoint_url = %endpoint.base_url(),
                attempt = attempt,
                max_retries = max_retries,
                "Router LLM returned empty response (0 text blocks received) - \
                 cannot determine routing decision (attempt {}/{})",
                attempt, max_retries
            );
            return Err(AppError::LlmRouting(LlmRouterError::EmptyResponse {
                endpoint: endpoint.base_url().to_string(),
            }));
        }

        tracing::debug!(
            endpoint_name = %endpoint.name(),
            response_length = response_text.len(),
            response = %response_text,
            attempt = attempt,
            "Received router decision from LLM"
        );

        // Parse routing decision
        Self::parse_routing_decision(&response_text)
    }

    /// Build router prompt from user request + metadata
    ///
    /// Creates a structured prompt that asks the LLM to choose between
    /// FAST, BALANCED, or DEEP based on the user's request and metadata.
    ///
    /// Includes prompt injection protection:
    /// - Truncates long user prompts to prevent context overflow
    /// - Adds reinforcement instructions after user input
    fn build_router_prompt(user_prompt: &str, meta: &RouteMetadata) -> String {
        // Truncate user prompt to prevent prompt injection via context overflow
        const MAX_USER_PROMPT_CHARS: usize = 500;

        // Use char-based indexing to avoid panics on UTF-8 boundaries
        let char_count = user_prompt.chars().count();
        let truncated_prompt = if char_count > MAX_USER_PROMPT_CHARS {
            let truncated: String = user_prompt.chars().take(MAX_USER_PROMPT_CHARS).collect();
            format!("{}... [truncated]", truncated)
        } else {
            user_prompt.to_string()
        };

        format!(
            "You are a router that chooses which LLM to use.\n\n\
             Available models:\n\
             - FAST: Quick (small params), for simple chat, short Q&A, casual tasks.\n\
             - BALANCED: Good reasoning (medium params), coding, document summaries, explanations.\n\
             - DEEP: Deep reasoning (large params), creative writing, complex analysis, research.\n\n\
             User request:\n{}\n\n\
             Metadata:\n\
             - Estimated tokens: {}\n\
             - Importance: {:?}\n\
             - Task type: {:?}\n\n\
             Based on the above, respond with ONLY one word: FAST, BALANCED, or DEEP.\n\
             Do not include explanations or other text.",
            truncated_prompt, meta.token_estimate, meta.importance, meta.task_type
        )
    }

    /// Find a word at word boundaries in text (prevents false positives)
    ///
    /// Returns the position of the first occurrence of `word` that is surrounded
    /// by word boundaries (whitespace, punctuation, or start/end of string).
    ///
    /// Prevents false positives like matching "FAST" in "BREAKFAST" or "STEADFAST".
    fn find_word_boundary(text: &str, word: &str) -> Option<usize> {
        let word_len = word.len();
        let text_bytes = text.as_bytes();

        // Try all possible positions where word could start
        for (pos, _) in text.match_indices(word) {
            // Check character before (must be word boundary or start of string)
            let before_is_boundary = if pos == 0 {
                true
            } else {
                // Check if previous character is non-alphanumeric (whitespace, punctuation, or non-ASCII).
                // Word boundary definition: Any character where is_ascii_alphanumeric() == false.
                // Examples:
                //   - "FAST-TRACK" matches "FAST" (dash is boundary)
                //   - "你FAST好" matches "FAST" (Chinese chars are boundary)
                //   - "steadFAST" does NOT match "FAST" (lowercase 'd' is alphanumeric)
                text_bytes[pos - 1].is_ascii_whitespace()
                    || !text_bytes[pos - 1].is_ascii_alphanumeric()
            };

            // Check character after (must be word boundary or end of string)
            let after_pos = pos + word_len;
            let after_is_boundary = if after_pos >= text.len() {
                true
            } else {
                // Check if next character is non-alphanumeric (whitespace, punctuation, or non-ASCII).
                // Word boundary definition: Any character where is_ascii_alphanumeric() == false.
                text_bytes[after_pos].is_ascii_whitespace()
                    || !text_bytes[after_pos].is_ascii_alphanumeric()
            };

            if before_is_boundary && after_is_boundary {
                return Some(pos);
            }
        }

        None
    }

    /// Parse LLM response to extract routing decision
    ///
    /// Uses **word-boundary-aware fuzzy matching** with refusal detection to extract
    /// FAST, BALANCED, or DEEP. Word boundaries are critical because:
    /// - Without them, "BREAKFAST" would match "FAST" (substring match)
    /// - Without them, "STEADFAST" would match "FAST" (substring match)
    /// - With boundaries, only whole-word matches succeed
    ///
    /// Prevents false positives like "FAST" in "BREAKFAST" by requiring keywords
    /// to be surrounded by word boundaries (whitespace, punctuation, or start/end of string).
    /// See `find_word_boundary()` for matching logic.
    ///
    /// Returns an error if response is empty, unparseable, or indicates refusal/error.
    ///
    /// Algorithm:
    /// 1. Check for refusal/error patterns (CANNOT, ERROR, UNABLE, SORRY) - return error
    /// 2. Find leftmost routing keyword (FAST, BALANCED, DEEP) at word boundary - return that tier
    /// 3. If no keyword found at word boundaries - return error (unparseable)
    ///
    /// Examples:
    /// - "FAST" → Fast (exact match)
    /// - "I recommend FAST for this" → Fast (word boundary match)
    /// - "FAST-TRACK" → Fast (punctuation counts as word boundary)
    /// - "BREAKFAST" → Error (no word boundary, substring ignored)
    /// - "FAST or BALANCED" → Fast (leftmost at word boundary wins)
    ///
    /// Errors indicate serious problems:
    /// - LLM misconfiguration (wrong model/prompt)
    /// - Safety filter activation
    /// - API failures or rate limiting
    /// - Prompt injection bypass
    fn parse_routing_decision(response: &str) -> AppResult<TargetModel> {
        let normalized = response.trim().to_uppercase();

        // Check for empty response first
        if normalized.is_empty() {
            tracing::error!(
                response = %response,
                "Router LLM returned empty response - cannot determine routing decision"
            );
            return Err(AppError::LlmRouting(LlmRouterError::EmptyResponse {
                endpoint: "router".to_string(),
            }));
        }

        // Check for refusal/error patterns BEFORE keyword matching
        //
        // Note: Uses simple substring matching - may have false positives if refusal
        // keywords appear in legitimate responses (e.g., "I CANNOT decide FAST enough").
        // This is acceptable because router responses should be single-word (FAST/BALANCED/DEEP)
        // per the prompt instructions. Any multi-word response indicates LLM malfunction
        // and should be treated as an error regardless.
        const REFUSAL_PATTERNS: &[&str] = &[
            "CANNOT", "CAN'T", "UNABLE", "ERROR", "SORRY", "REFUSE", "FAILED", "TIMEOUT",
        ];

        for pattern in REFUSAL_PATTERNS {
            if normalized.contains(pattern) {
                tracing::error!(
                    response = %response,
                    refusal_pattern = pattern,
                    "Router LLM returned refusal or error response"
                );

                // Truncate response to 500 chars for error message preview
                let response_preview = if response.len() > 500 {
                    format!("{}...", &response.chars().take(500).collect::<String>())
                } else {
                    response.to_string()
                };

                return Err(AppError::LlmRouting(LlmRouterError::Refusal {
                    endpoint: "router".to_string(),
                    message: format!(
                        "Router LLM returned refusal/error response (contains '{}'): '{}'",
                        pattern, response_preview
                    ),
                }));
            }
        }

        // Position-based matching with word boundary checking: Find leftmost routing keyword
        // This handles cases like "FAST or BALANCED" correctly (picks FAST)
        // Word boundary prevents false positives like "FAST" in "BREAKFAST"
        let fast_pos = Self::find_word_boundary(&normalized, "FAST");
        let balanced_pos = Self::find_word_boundary(&normalized, "BALANCED");
        let deep_pos = Self::find_word_boundary(&normalized, "DEEP");

        // Determine which keyword appears first (leftmost position)
        let positions = vec![
            (fast_pos, TargetModel::Fast),
            (balanced_pos, TargetModel::Balanced),
            (deep_pos, TargetModel::Deep),
        ];

        // Filter out None positions and find the minimum (leftmost)
        if let Some((_, model)) = positions
            .into_iter()
            .filter_map(|(pos, model)| pos.map(|p| (p, model)))
            .min_by_key(|(pos, _)| *pos)
        {
            return Ok(model);
        }

        // No valid routing decision found - return error instead of silent fallback
        // This indicates serious problems:
        // - LLM misconfiguration (wrong model, wrong prompt)
        // - Safety filter activation (LLM refusing to answer)
        // - API failures or rate limiting
        // - Prompt injection successful bypass
        tracing::error!(
            response = %response,
            response_length = response.len(),
            "Router LLM returned unparseable response - cannot extract FAST, BALANCED, or DEEP"
        );

        // Truncate response to 500 chars for error message preview
        let response_preview = if response.len() > 500 {
            format!(
                "{}... [truncated]",
                &response.chars().take(500).collect::<String>()
            )
        } else {
            response.to_string()
        };

        Err(AppError::LlmRouting(LlmRouterError::UnparseableResponse {
            endpoint: "router".to_string(),
            response: response_preview,
            response_length: response.len(),
        }))
    }
}

/// Implementation of LlmRouter trait for LlmBasedRouter
///
/// This allows LlmBasedRouter to be used as a trait object for dependency injection in tests.
#[async_trait]
impl LlmRouter for LlmBasedRouter {
    async fn route(&self, user_prompt: &str, meta: &RouteMetadata) -> AppResult<RoutingDecision> {
        // Delegate to the existing route method
        self.route(user_prompt, meta).await
    }
}

// Test modules
#[cfg(test)]
mod parsing_tests;

#[cfg(test)]
mod prompt_tests;

#[cfg(test)]
mod size_limit_tests;

#[cfg(test)]
mod utf8_safety_tests;

#[cfg(test)]
mod constructor_tests;

#[cfg(test)]
mod error_classification_tests;

#[cfg(test)]
mod error_type_tests;
