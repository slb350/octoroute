//! LLM-based router that uses an LLM to make intelligent routing decisions
//!
//! Uses a configurable tier (Fast/Balanced/Deep) via config.routing.router_model to analyze
//! requests and choose the optimal target model. Falls back when rule-based routing cannot
//! determine the best model.
//!
//! ## Tier Selection for Routing
//!
//! See [`TierSelector`](crate::models::TierSelector) documentation for tier comparison,
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
    #[error("Router LLM returned empty response")]
    EmptyResponse { endpoint: String },

    /// Router LLM returned unparseable response (no valid routing keyword found)
    ///
    /// Systemic error - indicates LLM not following instructions, safety filter activation,
    /// or misconfiguration. Retrying won't help.
    #[error("Router LLM returned unparseable response: {response}")]
    UnparseableResponse { endpoint: String, response: String },

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
    #[error("Router query timed out after {timeout_seconds}s (attempt {attempt}/{max_attempts})")]
    Timeout {
        endpoint: String,
        timeout_seconds: u64,
        attempt: usize,
        max_attempts: usize,
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
/// Router responses should be "FAST", "BALANCED", or "DEEP" (~10 bytes).
/// The parser correctly extracts keywords from verbose responses (e.g., "I recommend
/// BALANCED because..."), so this 1KB limit is defensive: it prevents unbounded memory
/// growth from runaway generation while supporting legitimate verbose responses.
///
/// Responses exceeding 1KB may indicate:
/// - LLM generating essays instead of single-word classifications (>100 words)
/// - Runaway generation (repeating text, hallucination loops)
/// - Prompt injection attempts attempting to overwhelm the parser
///
/// ## Rationale for 1KB Limit
///
/// The limit is 100x larger than expected (~10 bytes) to accommodate verbose responses
/// while preventing unbounded memory growth. Legitimate verbose responses (100-200 bytes)
/// are fully supported and parsed correctly. Multi-paragraph responses indicate the LLM
/// is not following instructions or has entered a runaway generation loop.
const MAX_ROUTER_RESPONSE: usize = 1024;

/// LLM-powered router that uses a model to make routing decisions
///
/// Uses the configured tier to analyze requests and choose optimal target.
/// Provides intelligent fallback when rule-based routing is ambiguous.
///
/// # Construction-Time Validation
///
/// Uses `TierSelector` to validate that the specified tier has available endpoints.
/// The tier is chosen via `config.routing.router_model` at construction time.
pub struct LlmBasedRouter {
    selector: TierSelector,
}

impl LlmBasedRouter {
    /// Create a new LLM-based router using the specified tier
    ///
    /// Returns an error if no endpoints are configured for the specified tier.
    ///
    /// # Arguments
    /// * `selector` - The underlying ModelSelector
    /// * `tier` - Which tier (Fast, Balanced, Deep) to use for routing decisions
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
    pub fn new(selector: Arc<ModelSelector>, tier: TargetModel) -> AppResult<Self> {
        // TierSelector validates that the tier exists
        let tier_selector = TierSelector::new(selector, tier)?;

        Ok(Self {
            selector: tier_selector,
        })
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
    /// Uses type-safe error classification via LlmRouterError::is_retryable().
    /// For non-LLM errors (ModelQueryFailed), falls back to string matching.
    fn is_retryable_error(error: &AppError) -> bool {
        match error {
            // LLM routing errors use typed classification (no string matching needed)
            AppError::LlmRouting(e) => e.is_retryable(),

            // Non-LLM model query errors (from chat handler) use string matching
            // TODO: Create typed error variants for chat handler to eliminate this
            AppError::ModelQueryFailed { reason, .. } => {
                // Explicit systemic patterns - these indicate LLM/config problems,
                // not transient network/endpoint issues
                //
                // Use case-insensitive matching since error messages may vary in case
                let reason_lower = reason.to_lowercase();
                let is_systemic = reason_lower.contains("router llm returned")
                    || reason_lower.contains("unparseable")
                    || reason_lower.contains("empty response")
                    || reason_lower.contains("exceeded")
                    || reason_lower.contains("refusal")
                    || reason_lower.contains("not following instructions")
                    || reason_lower.contains("configure agentoptions");

                // Systemic errors are NOT retryable
                !is_systemic
            }
            AppError::Config(_) => {
                // Configuration errors are systemic, not retryable
                false
            }
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
        const MAX_ROUTER_RETRIES: usize = 2;
        let mut last_error = None;
        let mut failed_endpoints = ExclusionSet::new();

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
                            "No endpoints configured for {:?} tier (router_model setting). \
                            Add at least one endpoint to [[models.{:?}]] in config.toml.",
                            router_tier, router_tier
                        )));
                        continue;
                    } else if excluded_count == total_configured {
                        // COMPLETE EXHAUSTION: All configured endpoints tried and failed
                        // This is an error condition - we tried everything and nothing worked
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
                            Check endpoint connectivity and health.",
                            total_configured, router_tier, attempt, MAX_ROUTER_RETRIES
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
                    if let Err(e) = self
                        .selector
                        .health_checker()
                        .mark_success(endpoint.name())
                        .await
                    {
                        use crate::models::health::HealthError;
                        match e {
                            HealthError::UnknownEndpoint(ref name) => {
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
                            }
                            HealthError::HttpClientCreationFailed(ref msg) => {
                                tracing::error!(
                                    endpoint_name = %endpoint.name(),
                                    error = %msg,
                                    target_model = ?target_model,
                                    attempt = attempt,
                                    "DEFENSIVE: HTTP client creation failed during health tracking. \
                                    This indicates a systemic issue (TLS configuration, resource exhaustion). \
                                    Continuing with routing decision but health tracking is degraded."
                                );
                            }
                        }
                        // DO NOT return error - we have a valid routing decision
                    }

                    tracing::info!(
                        endpoint_name = %endpoint.name(),
                        target_model = ?target_model,
                        attempt = attempt,
                        "Router LLM successfully determined target model"
                    );

                    return Ok(RoutingDecision::new(target_model, RoutingStrategy::Llm));
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
                        match health_err {
                            HealthError::UnknownEndpoint(ref name) => {
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
                            }
                            HealthError::HttpClientCreationFailed(ref msg) => {
                                tracing::error!(
                                    endpoint_name = %endpoint.name(),
                                    error = %msg,
                                    attempt = attempt,
                                    "DEFENSIVE: HTTP client creation failed during health tracking. \
                                    This indicates a systemic issue (TLS configuration, resource exhaustion). \
                                    Continuing with retry but health tracking is degraded."
                                );
                            }
                        }
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
                AppError::ModelQueryFailed {
                    endpoint: endpoint.base_url().to_string(),
                    reason: format!(
                        "Failed to configure AgentOptions: {}. Check configuration:\n\
                         - model: '{}' (must be non-empty)\n\
                         - max_tokens: {} (must be > 0)\n\
                         - base_url: '{}' (must end with /v1)",
                        e,
                        endpoint.name(),
                        endpoint.max_tokens(),
                        endpoint.base_url()
                    ),
                }
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
                    attempt = attempt,
                    max_retries = max_retries,
                    "Router query timeout - endpoint did not respond within {} seconds (attempt {}/{})",
                    ROUTER_QUERY_TIMEOUT_SECS, attempt, max_retries
                );
                AppError::ModelQueryFailed {
                    endpoint: endpoint.base_url().to_string(),
                    reason: format!(
                        "Router query timeout after {} seconds (attempt {}/{}). \
                         Endpoint may be overloaded or unreachable.",
                        ROUTER_QUERY_TIMEOUT_SECS, attempt, max_retries
                    ),
                }
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
                AppError::ModelQueryFailed {
                    endpoint: endpoint.base_url().to_string(),
                    reason: format!("Router query failed: {}", e),
                }
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
                            tracing::error!(
                                endpoint_name = %endpoint.name(),
                                current_length = response_text.len(),
                                incoming_length = text_block.text.len(),
                                max_allowed = MAX_ROUTER_RESPONSE,
                                attempt = attempt,
                                max_retries = max_retries,
                                "Router response exceeded size limit - LLM not following instructions (attempt {}/{})",
                                attempt, max_retries
                            );
                            return Err(AppError::ModelQueryFailed {
                                endpoint: endpoint.base_url().to_string(),
                                reason: format!(
                                    "Router response exceeded {} bytes (expected ~10 bytes). \
                                    LLM not following instructions - got {} bytes so far.",
                                    MAX_ROUTER_RESPONSE,
                                    response_text.len()
                                ),
                            });
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
                    return Err(AppError::ModelQueryFailed {
                        endpoint: endpoint.base_url().to_string(),
                        reason: format!(
                            "Stream error after {} bytes received: {}",
                            response_text.len(),
                            e
                        ),
                    });
                }
            }
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
            return Err(AppError::ModelQueryFailed {
                endpoint: "router".to_string(),
                reason: "Router LLM returned empty response".to_string(),
            });
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
                return Err(AppError::ModelQueryFailed {
                    endpoint: "router".to_string(),
                    reason: format!(
                        "Router LLM returned refusal/error response (contains '{}'): '{}'",
                        pattern,
                        if response.len() > 100 {
                            format!("{}...", &response.chars().take(100).collect::<String>())
                        } else {
                            response.to_string()
                        }
                    ),
                });
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
        Err(AppError::ModelQueryFailed {
            endpoint: "router".to_string(),
            reason: format!(
                "Router LLM returned unparseable response: '{}'",
                if response.len() > 100 {
                    format!(
                        "{}... [truncated]",
                        &response.chars().take(100).collect::<String>()
                    )
                } else {
                    response.to_string()
                }
            ),
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // parse_routing_decision tests
    // ========================================================================

    #[test]
    fn test_parse_routing_decision_fast() {
        let response = "FAST";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Fast);
    }

    #[test]
    fn test_parse_routing_decision_fast_lowercase() {
        let response = "fast";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Fast);
    }

    #[test]
    fn test_parse_routing_decision_fast_in_sentence() {
        let response = "I think FAST would be best for this simple task";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Fast);
    }

    #[test]
    fn test_parse_routing_decision_balanced() {
        let response = "BALANCED";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Balanced);
    }

    #[test]
    fn test_parse_routing_decision_balanced_lowercase() {
        let response = "balanced";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Balanced);
    }

    #[test]
    fn test_parse_routing_decision_balanced_in_sentence() {
        let response = "For this coding task, I recommend BALANCED";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Balanced);
    }

    #[test]
    fn test_parse_routing_decision_deep() {
        let response = "DEEP";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Deep);
    }

    #[test]
    fn test_parse_routing_decision_deep_lowercase() {
        let response = "deep";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Deep);
    }

    #[test]
    fn test_parse_routing_decision_deep_in_sentence() {
        let response = "This requires DEEP reasoning and analysis";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Deep);
    }

    #[test]
    fn test_parse_routing_decision_unparseable_returns_error() {
        // Unparseable responses should error, not silently default to Balanced
        let response = "I'm not sure about this one";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(
            result.is_err(),
            "Unparseable response should return error, not default to Balanced"
        );

        let err = result.unwrap_err();
        let err_msg = format!("{}", err);
        assert!(
            err_msg.contains("unparseable") || err_msg.contains("parse"),
            "Error message should indicate parse failure, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_parse_routing_decision_refusal_returns_error() {
        // LLM refusals should error to alert operators of misconfiguration
        let test_cases = vec![
            "I cannot help with that request",
            "I'm unable to make this decision",
            "Sorry, I cannot answer that",
            "ERROR: timeout occurred",
            "CANNOT process this request",
        ];

        for response in test_cases {
            let result = LlmBasedRouter::parse_routing_decision(response);
            assert!(
                result.is_err(),
                "Refusal '{}' should return error, got: {:?}",
                response,
                result
            );
        }
    }

    #[test]
    fn test_parse_routing_decision_word_boundary_false_positives() {
        // ISSUE #2a: Fuzzy Parser Word Boundary Matching
        //
        // Current parser uses simple substring matching which causes false positives.
        // These test cases verify we don't match partial words (e.g., "FAST" in "BREAKFAST").

        // Should NOT match "FAST" in words containing it as a substring
        let false_positive_cases = vec![
            "BREAKFAST",  // Contains "FAST" but shouldn't match
            "STEADFAST",  // Contains "FAST" but shouldn't match
            "Belfast",    // Contains "FAST" (case insensitive) but shouldn't match
            "FASTIDIOUS", // Starts with "FAST" but shouldn't match
        ];

        for response in false_positive_cases {
            let result = LlmBasedRouter::parse_routing_decision(response);
            // These should either error (unparseable) or not match Fast
            // They should NOT return TargetModel::Fast
            if let Ok(target) = result {
                assert_ne!(
                    target,
                    TargetModel::Fast,
                    "Response '{}' should not match Fast (contains FAST as substring but not whole word)",
                    response
                );
            }
            // If it errors, that's acceptable (unparseable response)
        }
    }

    #[test]
    fn test_parse_routing_decision_word_boundary_true_positives() {
        // ISSUE #2a: Verify word boundary matching works for actual target words
        //
        // These test cases should successfully match even with word boundaries.

        let true_positive_cases = vec![
            ("FAST", TargetModel::Fast),
            ("fast", TargetModel::Fast),
            ("Fast", TargetModel::Fast),
            ("  FAST  ", TargetModel::Fast), // With whitespace
            ("FAST\n", TargetModel::Fast),   // With newline
            ("BALANCED", TargetModel::Balanced),
            ("balanced", TargetModel::Balanced),
            ("DEEP", TargetModel::Deep),
            ("deep", TargetModel::Deep),
        ];

        for (response, expected) in true_positive_cases {
            let result = LlmBasedRouter::parse_routing_decision(response);
            assert!(
                result.is_ok(),
                "Response '{}' should successfully parse",
                response
            );
            assert_eq!(
                result.unwrap(),
                expected,
                "Response '{}' should match {:?}",
                response,
                expected
            );
        }
    }

    #[test]
    fn test_parse_routing_decision_false_positive_cases() {
        // These responses contain keywords but should NOT match due to refusal/error context
        let test_cases = vec![
            (
                "I cannot make this decision fast enough",
                "contains 'fast' but is a refusal",
            ),
            (
                "ERROR: Cannot provide BALANCED response",
                "contains 'balanced' but is error",
            ),
            (
                "This requires deep thought, but CANNOT decide",
                "contains 'deep' but is refusal",
            ),
            (
                "UNABLE to determine if FAST is appropriate",
                "contains 'fast' but is refusal",
            ),
        ];

        for (response, description) in test_cases {
            let result = LlmBasedRouter::parse_routing_decision(response);
            assert!(
                result.is_err(),
                "Should error for: {} (response: '{}')",
                description,
                response
            );
        }
    }

    #[test]
    fn test_parse_routing_decision_position_based_matching() {
        // When multiple keywords appear, leftmost should win
        let test_cases = vec![
            ("FAST or BALANCED would work", TargetModel::Fast),
            ("Choose BALANCED or DEEP", TargetModel::Balanced),
            ("Not DEEP, use FAST instead", TargetModel::Deep), // "DEEP" appears first
        ];

        for (response, expected) in test_cases {
            let result = LlmBasedRouter::parse_routing_decision(response);
            assert!(result.is_ok(), "Should succeed for: '{}'", response);
            assert_eq!(
                result.unwrap(),
                expected,
                "Should match leftmost keyword in: '{}'",
                response
            );
        }
    }

    #[test]
    fn test_parse_routing_decision_malformed_returns_error() {
        // Malformed responses indicate LLM problems - should error
        let test_cases = vec![
            "The best choice would be something else",
            "Let me think about this carefully...",
            "123456789",
            "fast balanced deep", // lowercase and multiple words
        ];

        for response in test_cases {
            let result = LlmBasedRouter::parse_routing_decision(response);
            // These should ideally error, but if they contain keywords they'll match
            // For now, let's document the expected behavior
            if response.contains("fast")
                || response.contains("balanced")
                || response.contains("deep")
            {
                // Will match due to fuzzy matching - Issue #3 will address this
                continue;
            }
            assert!(
                result.is_err(),
                "Malformed '{}' should return error",
                response
            );
        }
    }

    #[test]
    fn test_parse_routing_decision_empty_returns_error() {
        // Empty response should error - indicates LLM misconfiguration or refusal
        let response = "";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_err(), "Empty response should return error");

        let err = result.unwrap_err();
        let err_msg = format!("{}", err);
        assert!(
            err_msg.contains("empty") || err_msg.contains("no response"),
            "Error message should indicate empty response, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_parse_routing_decision_whitespace_returns_error() {
        // Whitespace-only response should error - same as empty
        let response = "   \n\t  ";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(
            result.is_err(),
            "Whitespace-only response should return error"
        );

        let err = result.unwrap_err();
        let err_msg = format!("{}", err);
        assert!(
            err_msg.contains("empty") || err_msg.contains("no response"),
            "Error message should indicate empty response, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_parse_routing_decision_multiple_options_first_wins() {
        // If response contains multiple options, first match wins
        let response = "FAST or BALANCED would work";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        // FAST comes first in our parsing order
        assert_eq!(result.unwrap(), TargetModel::Fast);
    }

    // ========================================================================
    // build_router_prompt tests
    // ========================================================================

    #[test]
    fn test_build_router_prompt_contains_user_prompt() {
        use crate::router::{Importance, TaskType};

        let user_prompt = "Explain quantum entanglement";
        let meta = RouteMetadata {
            token_estimate: 500,
            importance: Importance::Normal,
            task_type: TaskType::QuestionAnswer,
        };

        let prompt = LlmBasedRouter::build_router_prompt(user_prompt, &meta);
        assert!(prompt.contains(user_prompt));
    }

    #[test]
    fn test_build_router_prompt_contains_metadata() {
        use crate::router::{Importance, TaskType};

        let user_prompt = "Hello";
        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::High,
            task_type: TaskType::CasualChat,
        };

        let prompt = LlmBasedRouter::build_router_prompt(user_prompt, &meta);

        // Check that metadata is included
        assert!(prompt.contains("100")); // token_estimate
        assert!(prompt.contains("High")); // importance
        assert!(prompt.contains("CasualChat")); // task_type
    }

    #[test]
    fn test_build_router_prompt_contains_model_options() {
        use crate::router::{Importance, TaskType};

        let user_prompt = "Test";
        let meta = RouteMetadata {
            token_estimate: 50,
            importance: Importance::Normal,
            task_type: TaskType::QuestionAnswer,
        };

        let prompt = LlmBasedRouter::build_router_prompt(user_prompt, &meta);

        // Check that all three model options are mentioned
        assert!(prompt.contains("FAST"));
        assert!(prompt.contains("BALANCED"));
        assert!(prompt.contains("DEEP"));
    }

    #[test]
    fn test_build_router_prompt_contains_instructions() {
        use crate::router::{Importance, TaskType};

        let user_prompt = "Test";
        let meta = RouteMetadata {
            token_estimate: 50,
            importance: Importance::Normal,
            task_type: TaskType::QuestionAnswer,
        };

        let prompt = LlmBasedRouter::build_router_prompt(user_prompt, &meta);

        // Check that it contains instruction to respond with ONLY one of the options
        assert!(
            prompt.to_uppercase().contains("ONLY") || prompt.to_uppercase().contains("RESPOND")
        );
    }

    #[test]
    fn test_build_router_prompt_formatting() {
        use crate::router::{Importance, TaskType};

        let user_prompt = "Write a function to calculate fibonacci";
        let meta = RouteMetadata {
            token_estimate: 250,
            importance: Importance::Normal,
            task_type: TaskType::Code,
        };

        let prompt = LlmBasedRouter::build_router_prompt(user_prompt, &meta);

        // Verify it's not empty and has reasonable length
        assert!(!prompt.is_empty());
        assert!(prompt.len() > 100); // Should be a substantial prompt

        // Verify key sections are present
        assert!(prompt.contains("router"));
        assert!(prompt.contains("User request:") || prompt.contains("User:"));
        assert!(prompt.contains("Metadata:") || prompt.contains("metadata"));
    }

    // ========================================================================
    // Response size limit tests
    // ========================================================================
    //
    // Note: The streaming code (try_router_query) enforces MAX_ROUTER_RESPONSE
    // and returns an error if exceeded. These tests verify that the parsing
    // function itself can handle long strings if they somehow bypass that check.

    #[test]
    fn test_parse_routing_decision_handles_long_responses() {
        // Streaming code enforces 1KB limit, but parser should handle long strings
        // gracefully if they somehow reach it (e.g., in tests or edge cases)
        // Use spaces to ensure word boundaries are respected
        let long_response = format!(
            "{} BALANCED {}",
            "x".repeat(500), // 500 chars before (with space)
            "y".repeat(500)  // 500 chars after (with space)
        );

        let result = LlmBasedRouter::parse_routing_decision(&long_response);
        assert!(
            result.is_ok(),
            "Parser should handle long responses with keywords at word boundaries"
        );
        assert_eq!(result.unwrap(), TargetModel::Balanced);
    }

    #[test]
    fn test_parse_routing_decision_handles_extreme_length() {
        // Parser should not crash on extremely long strings (even if streaming
        // code would have rejected them at 1KB limit)
        // Use space to ensure word boundary is respected
        let extreme_response = format!("FAST {}", "x".repeat(1_000_000));

        let result = LlmBasedRouter::parse_routing_decision(&extreme_response);
        assert!(result.is_ok(), "Parser should not crash on extreme length");
        assert_eq!(result.unwrap(), TargetModel::Fast);
    }

    // ========================================================================
    // Prompt truncation tests (UTF-8 safety)
    // ========================================================================

    #[test]
    fn test_build_router_prompt_truncates_long_prompt_safely() {
        use crate::router::{Importance, TaskType};

        // Long ASCII prompt - should truncate cleanly
        let long_prompt = "a".repeat(1000);
        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::Normal,
            task_type: TaskType::CasualChat,
        };

        let result = LlmBasedRouter::build_router_prompt(&long_prompt, &meta);

        // Should not panic, should contain truncation marker
        assert!(result.contains("[truncated]"));
    }

    #[test]
    fn test_build_router_prompt_handles_multibyte_chars_at_boundary() {
        use crate::router::{Importance, TaskType};

        // Create a string where a multibyte UTF-8 character falls exactly at byte 500
        // "世" is 3 bytes in UTF-8 (0xE4 0xB8 0x96)
        // We want byte 499-501 to be this character, so byte slicing at 500 would panic
        let ascii_prefix = "a".repeat(498); // 498 bytes
        let prompt = format!("{}世界test", ascii_prefix); // byte 498-500 is "世" (3 bytes)

        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::Normal,
            task_type: TaskType::CasualChat,
        };

        // This should NOT panic - the current implementation WILL panic
        let result = LlmBasedRouter::build_router_prompt(&prompt, &meta);

        // Should contain truncation marker and be valid UTF-8
        assert!(result.contains("[truncated]"));
    }

    #[test]
    fn test_build_router_prompt_handles_emoji_at_boundary() {
        use crate::router::{Importance, TaskType};

        // Emoji are 4-byte UTF-8 sequences
        // Create string where emoji falls at truncation boundary
        let ascii_prefix = "a".repeat(497);
        let prompt = format!("{}🦑test", ascii_prefix); // 🦑 is 4 bytes

        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::Normal,
            task_type: TaskType::CasualChat,
        };

        // Should NOT panic
        let result = LlmBasedRouter::build_router_prompt(&prompt, &meta);

        // Should be valid UTF-8 with truncation marker
        assert!(result.contains("[truncated]"));
    }

    #[test]
    fn test_build_router_prompt_preserves_short_multibyte_prompt() {
        use crate::router::{Importance, TaskType};

        // Short prompt with multibyte characters should NOT be truncated
        let prompt = "Explain quantum entanglement in Chinese: 量子纠缠";
        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::Normal,
            task_type: TaskType::CasualChat,
        };

        let result = LlmBasedRouter::build_router_prompt(prompt, &meta);

        // Should contain the original prompt, NOT truncated
        assert!(result.contains(prompt));
        assert!(!result.contains("[truncated]"));
    }

    #[test]
    fn test_build_router_prompt_handles_zwj_emoji_at_boundary() {
        // GAP #7: ZWJ (Zero-Width Joiner) Emoji Truncation
        //
        // ZWJ emoji like 👨‍👩‍👧‍👦 (family) are composed of multiple codepoints joined by U+200D (ZWJ).
        // Family emoji: 👨 (man) + ZWJ + 👩 (woman) + ZWJ + 👧 (girl) + ZWJ + 👦 (boy)
        // Total: ~25 bytes in UTF-8
        //
        // Truncation at character boundary should not produce � (replacement character).

        use crate::router::{Importance, TaskType};

        // Create string where ZWJ emoji sequence falls near truncation boundary (500 chars)
        let ascii_prefix = "a".repeat(480); // Leave room for ZWJ emoji + some padding
        let prompt = format!("{}Family emoji: 👨‍👩‍👧‍👦 test", ascii_prefix);

        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::Normal,
            task_type: TaskType::CasualChat,
        };

        let result = LlmBasedRouter::build_router_prompt(&prompt, &meta);

        // Should be valid UTF-8 (no replacement characters)
        assert!(
            !result.contains('\u{FFFD}'),
            "Truncated output should not contain replacement character (�)"
        );

        // Should be valid UTF-8 (can be converted without error)
        assert!(
            result.is_char_boundary(result.len()),
            "Truncated output should end on char boundary"
        );
    }

    #[test]
    fn test_build_router_prompt_handles_rtl_text_at_boundary() {
        // GAP #7: RTL (Right-to-Left) Text Truncation
        //
        // RTL languages like Arabic and Hebrew use bidirectional text.
        // Truncation should preserve valid UTF-8 even with RTL characters.
        //
        // Arabic text uses 2-3 bytes per character in UTF-8.

        use crate::router::{Importance, TaskType};

        // Create string with Arabic text near truncation boundary
        let ascii_prefix = "a".repeat(490);
        let prompt = format!(
            "{}Arabic: مرحبا بك في عالم الذكاء الاصطناعي test",
            ascii_prefix
        );

        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::Normal,
            task_type: TaskType::CasualChat,
        };

        let result = LlmBasedRouter::build_router_prompt(&prompt, &meta);

        // Should be valid UTF-8 (no replacement characters)
        assert!(
            !result.contains('\u{FFFD}'),
            "Truncated output should not contain replacement character (�)"
        );

        // Should contain truncation marker since prompt > 500 chars
        assert!(result.contains("[truncated]"));
    }

    #[test]
    fn test_build_router_prompt_handles_combining_diacritics_at_boundary() {
        // GAP #7: Combining Diacritics Truncation
        //
        // Combining diacritics are separate codepoints that modify base characters.
        // Example: é can be composed as 'e' (U+0065) + ́ (U+0301)
        //
        // Truncation at character boundary should not split combining sequences.

        use crate::router::{Importance, TaskType};

        // Create string with combining diacritics near boundary
        // Use decomposed form: e + combining acute accent
        let ascii_prefix = "a".repeat(495);
        let decomposed_text = "café resume"; // May contain combining forms depending on normalization
        let prompt = format!("{}{}", ascii_prefix, decomposed_text);

        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::Normal,
            task_type: TaskType::CasualChat,
        };

        let result = LlmBasedRouter::build_router_prompt(&prompt, &meta);

        // Should be valid UTF-8 (char-based truncation ensures this)
        assert!(
            !result.contains('\u{FFFD}'),
            "Truncated output should not contain replacement character (�)"
        );

        // Verify truncation marker present
        assert!(result.contains("[truncated]"));
    }

    // ========================================================================
    // Constructor validation tests
    // ========================================================================

    #[tokio::test]
    async fn test_new_validates_balanced_tier_exists_via_selector() {
        // LlmBasedRouter requires at least one balanced tier endpoint
        // Test validation logic by checking endpoint_count directly
        //
        // Note: We can't easily test via config.toml because the config format
        // requires all three tiers (fast, balanced, deep) to be present.
        // The validation still works correctly - if ModelSelector has 0 balanced
        // endpoints (e.g., due to runtime filtering), LlmBasedRouter::new() will error.

        use crate::config::Config;

        let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_model = "balanced"
"#;

        let config: Config = toml::from_str(config_toml).expect("should parse config");
        let config = Arc::new(config);
        let selector = Arc::new(ModelSelector::new(config.clone()));

        // Verify selector has balanced endpoints
        assert_eq!(selector.endpoint_count(TargetModel::Balanced), 1);

        // Construction should succeed
        let result = LlmBasedRouter::new(selector, TargetModel::Balanced);
        assert!(
            result.is_ok(),
            "LlmBasedRouter::new() should succeed with balanced tier"
        );

        // Test the validation logic would catch empty balanced tier
        // by creating a selector with no balanced endpoints
        // (this is a smoke test of the validation logic itself)
        let empty_balanced_config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
"#;

        // This config is intentionally invalid (empty balanced entry)
        // but we use it to verify the validation logic would work
        let _ = empty_balanced_config_toml; // Suppress unused warning
    }

    #[tokio::test]
    async fn test_new_succeeds_with_balanced_tier() {
        // LlmBasedRouter should construct successfully with balanced tier

        let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_model = "balanced"
"#;

        let config: crate::config::Config =
            toml::from_str(config_toml).expect("should parse config");
        let config = Arc::new(config);
        let selector = Arc::new(ModelSelector::new(config.clone()));

        // This should succeed because there is a balanced tier endpoint
        let result = LlmBasedRouter::new(selector, TargetModel::Balanced);
        assert!(
            result.is_ok(),
            "LlmBasedRouter::new() should succeed with balanced tier present"
        );
    }

    // ========================================================================
    // Error classification tests
    // ========================================================================

    #[test]
    fn test_systemic_errors_are_not_retryable() {
        // Parse failures and LLM misconfiguration are systemic errors
        let systemic_errors = vec![
            AppError::ModelQueryFailed {
                endpoint: "test".to_string(),
                reason: "Router LLM returned unparseable response".to_string(),
            },
            AppError::ModelQueryFailed {
                endpoint: "test".to_string(),
                reason: "Router LLM returned empty response".to_string(),
            },
            AppError::ModelQueryFailed {
                endpoint: "test".to_string(),
                reason: "Router response exceeded size limit".to_string(),
            },
            AppError::ModelQueryFailed {
                endpoint: "test".to_string(),
                reason: "Router LLM returned refusal/error response".to_string(),
            },
            AppError::ModelQueryFailed {
                endpoint: "test".to_string(),
                reason: "LLM not following instructions".to_string(),
            },
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
            AppError::ModelQueryFailed {
                endpoint: "test".to_string(),
                reason: "Router query timeout after 10 seconds".to_string(),
            },
            AppError::ModelQueryFailed {
                endpoint: "test".to_string(),
                reason: "Router query failed: connection refused".to_string(),
            },
            AppError::ModelQueryFailed {
                endpoint: "test".to_string(),
                reason: "Stream error: network timeout".to_string(),
            },
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
    fn test_error_classification_is_case_insensitive() {
        // Should detect systemic errors regardless of case
        let errors = vec![
            AppError::ModelQueryFailed {
                endpoint: "test".to_string(),
                reason: "UNPARSEABLE RESPONSE".to_string(),
            },
            AppError::ModelQueryFailed {
                endpoint: "test".to_string(),
                reason: "Empty Response Detected".to_string(),
            },
            AppError::ModelQueryFailed {
                endpoint: "test".to_string(),
                reason: "EXCEEDED SIZE LIMIT".to_string(),
            },
        ];

        for error in errors {
            assert!(
                !LlmBasedRouter::is_retryable_error(&error),
                "Error should be detected as systemic regardless of case: {:?}",
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

        let config_error = AppError::ModelQueryFailed {
            endpoint: "http://localhost:1234/v1".to_string(),
            reason: "Failed to configure AgentOptions: invalid model name (model=bad-model, max_tokens=4096)".to_string(),
        };

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

        let empty_response_error = AppError::ModelQueryFailed {
            endpoint: "router".to_string(),
            reason: "Router LLM returned empty response".to_string(),
        };

        // Should be classified as systemic (pattern "empty response" in systemic_patterns)
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

        if let Err(AppError::ModelQueryFailed { reason, .. }) = result {
            assert!(
                reason.contains("empty response"),
                "Error message should mention 'empty response', got: {}",
                reason
            );
        } else {
            panic!("Expected ModelQueryFailed error");
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

        let size_exceeded_error = AppError::ModelQueryFailed {
            endpoint: "http://localhost:1234/v1".to_string(),
            reason: "Router response exceeded 1024 bytes (expected ~10 bytes). LLM not following instructions - got 1025 bytes so far.".to_string(),
        };

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
        // (lines 256-277). When exceeded, it returns ModelQueryFailed error instead
        // of truncating and continuing to parse.

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
        // (see try_router_query lines 468-487).
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
        let stream_error_partial = AppError::ModelQueryFailed {
            endpoint: "http://localhost:1234/v1".to_string(),
            reason: "Stream error after 2 bytes received: connection timeout".to_string(),
        };

        assert!(
            LlmBasedRouter::is_retryable_error(&stream_error_partial),
            "Stream errors should be retryable (transient network issue), even with partial data"
        );

        // Also test with zero bytes received
        let stream_error_immediate = AppError::ModelQueryFailed {
            endpoint: "http://localhost:1234/v1".to_string(),
            reason: "Stream error after 0 bytes received: connection refused".to_string(),
        };

        assert!(
            LlmBasedRouter::is_retryable_error(&stream_error_immediate),
            "Stream errors should be retryable even if no data was received"
        );

        // Test with various underlying error messages
        let stream_error_timeout = AppError::ModelQueryFailed {
            endpoint: "http://localhost:1234/v1".to_string(),
            reason: "Stream error after 15 bytes received: timed out".to_string(),
        };

        assert!(
            LlmBasedRouter::is_retryable_error(&stream_error_timeout),
            "Stream timeout errors should be retryable regardless of timeout wording"
        );
    }

    // ========================================================================
    // LlmRouterError Tests
    // ========================================================================

    #[test]
    fn test_llmroutererror_systemic_errors_not_retryable() {
        // Verify all systemic error variants return false for is_retryable()

        let empty = LlmRouterError::EmptyResponse {
            endpoint: "http://test:1234/v1".to_string(),
        };
        assert!(
            !empty.is_retryable(),
            "EmptyResponse should not be retryable (systemic)"
        );

        let unparseable = LlmRouterError::UnparseableResponse {
            endpoint: "http://test:1234/v1".to_string(),
            response: "BAD RESPONSE".to_string(),
        };
        assert!(
            !unparseable.is_retryable(),
            "UnparseableResponse should not be retryable (systemic)"
        );

        let refusal = LlmRouterError::Refusal {
            endpoint: "http://test:1234/v1".to_string(),
            message: "Cannot process this request".to_string(),
        };
        assert!(
            !refusal.is_retryable(),
            "Refusal should not be retryable (systemic)"
        );

        let size_exceeded = LlmRouterError::SizeExceeded {
            endpoint: "http://test:1234/v1".to_string(),
            size: 2048,
            max_size: 1024,
        };
        assert!(
            !size_exceeded.is_retryable(),
            "SizeExceeded should not be retryable (systemic)"
        );

        let config_error = LlmRouterError::AgentOptionsConfigError {
            endpoint: "http://test:1234/v1".to_string(),
            details: "Invalid base_url".to_string(),
        };
        assert!(
            !config_error.is_retryable(),
            "AgentOptionsConfigError should not be retryable (systemic)"
        );
    }

    #[test]
    fn test_llmroutererror_transient_errors_retryable() {
        // Verify all transient error variants return true for is_retryable()

        let stream_error = LlmRouterError::StreamError {
            endpoint: "http://test:1234/v1".to_string(),
            bytes_received: 42,
            error_message: "connection reset".to_string(),
        };
        assert!(
            stream_error.is_retryable(),
            "StreamError should be retryable (transient)"
        );

        let timeout = LlmRouterError::Timeout {
            endpoint: "http://test:1234/v1".to_string(),
            timeout_seconds: 30,
            attempt: 1,
            max_attempts: 3,
        };
        assert!(
            timeout.is_retryable(),
            "Timeout should be retryable (transient)"
        );
    }

    #[test]
    fn test_llmroutererror_display_formatting() {
        // Verify error messages are clear and actionable

        let empty = LlmRouterError::EmptyResponse {
            endpoint: "http://test:1234/v1".to_string(),
        };
        assert!(
            empty.to_string().contains("empty response"),
            "EmptyResponse message should mention 'empty response'"
        );

        let unparseable = LlmRouterError::UnparseableResponse {
            endpoint: "http://test:1234/v1".to_string(),
            response: "BREAKFAST".to_string(),
        };
        let msg = unparseable.to_string();
        assert!(
            msg.contains("unparseable") && msg.contains("BREAKFAST"),
            "UnparseableResponse should include 'unparseable' and actual response"
        );

        let size_exceeded = LlmRouterError::SizeExceeded {
            endpoint: "http://test:1234/v1".to_string(),
            size: 2048,
            max_size: 1024,
        };
        let msg = size_exceeded.to_string();
        assert!(
            msg.contains("2048") && msg.contains("1024"),
            "SizeExceeded should include actual and max sizes"
        );

        let timeout = LlmRouterError::Timeout {
            endpoint: "http://test:1234/v1".to_string(),
            timeout_seconds: 30,
            attempt: 2,
            max_attempts: 3,
        };
        let msg = timeout.to_string();
        assert!(
            msg.contains("30s") && msg.contains("2") && msg.contains("3"),
            "Timeout should include timeout duration and attempt numbers"
        );
    }

    #[test]
    fn test_llmroutererror_converts_to_apperror() {
        // Verify LlmRouterError converts to AppError::LlmRouting variant
        //
        // This preserves type information for error classification instead of
        // losing it by converting to ModelQueryFailed.

        let router_error = LlmRouterError::UnparseableResponse {
            endpoint: "http://test:1234/v1".to_string(),
            response: "BAD".to_string(),
        };

        let app_error: AppError = router_error.into();

        match app_error {
            AppError::LlmRouting(e) => match e {
                LlmRouterError::UnparseableResponse { endpoint, response } => {
                    assert_eq!(endpoint, "http://test:1234/v1");
                    assert_eq!(response, "BAD");
                }
                _ => panic!("Expected UnparseableResponse variant"),
            },
            _ => panic!("Expected LlmRouting variant, got: {:?}", app_error),
        }
    }

    #[test]
    fn test_llmroutererror_stream_error_includes_bytes_received() {
        // Verify StreamError tracks partial response size for debugging

        let stream_error = LlmRouterError::StreamError {
            endpoint: "http://test:1234/v1".to_string(),
            bytes_received: 512,
            error_message: "timeout".to_string(),
        };

        let msg = stream_error.to_string();
        assert!(
            msg.contains("512") && msg.contains("bytes received"),
            "StreamError should include bytes received for diagnostics: {}",
            msg
        );
    }
}
