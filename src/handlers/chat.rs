//! Chat endpoint handler
//!
//! Handles POST /chat requests with intelligent model routing.

use crate::config::ModelEndpoint;
use crate::error::{AppError, AppResult};
use crate::handlers::AppState;
use crate::middleware::RequestId;
use crate::models::{EndpointName, ExclusionSet};
use crate::router::{Importance, RouteMetadata, RoutingStrategy, TargetModel, TaskType};
use axum::{Extension, Json, extract::State, response::IntoResponse};
use serde::{Deserialize, Deserializer, Serialize};
use std::time::Duration;

/// Maximum allowed message length in characters (100K chars)
const MAX_MESSAGE_LENGTH: usize = 100_000;

/// Chat request from client
///
/// Validation is enforced during deserialization - invalid instances cannot exist.
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    message: String,
    importance: Importance,
    task_type: TaskType,
}

impl ChatRequest {
    /// Get the message
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Get the importance level
    pub fn importance(&self) -> Importance {
        self.importance
    }

    /// Get the task type
    pub fn task_type(&self) -> TaskType {
        self.task_type
    }

    /// Convert request to RouteMetadata for routing decisions
    pub fn to_metadata(&self) -> RouteMetadata {
        let token_estimate = RouteMetadata::estimate_tokens(&self.message);
        RouteMetadata {
            token_estimate,
            importance: self.importance,
            task_type: self.task_type,
        }
    }
}

/// Custom Deserialize implementation that validates during deserialization
impl<'de> Deserialize<'de> for ChatRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawChatRequest {
            message: String,
            #[serde(default)]
            importance: Importance,
            #[serde(default)]
            task_type: TaskType,
        }

        let raw = RawChatRequest::deserialize(deserializer)?;

        // Validate message is not empty or whitespace-only
        if raw.message.trim().is_empty() {
            return Err(serde::de::Error::custom(
                "message cannot be empty or contain only whitespace",
            ));
        }

        // Validate message length (count Unicode characters, not bytes)
        let char_count = raw.message.chars().count();
        if char_count > MAX_MESSAGE_LENGTH {
            return Err(serde::de::Error::custom(format!(
                "message exceeds maximum length of {} characters (got {})",
                MAX_MESSAGE_LENGTH, char_count
            )));
        }

        Ok(ChatRequest {
            message: raw.message,
            importance: raw.importance,
            task_type: raw.task_type,
        })
    }
}

/// Model tier classification for API responses
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    Fast,
    Balanced,
    Deep,
}

impl From<crate::router::TargetModel> for ModelTier {
    fn from(target: crate::router::TargetModel) -> Self {
        match target {
            crate::router::TargetModel::Fast => ModelTier::Fast,
            crate::router::TargetModel::Balanced => ModelTier::Balanced,
            crate::router::TargetModel::Deep => ModelTier::Deep,
        }
    }
}

/// Chat response to client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Model's response content
    pub content: String,
    /// Which model tier was used
    pub model_tier: ModelTier,
    /// Which specific endpoint was used
    pub model_name: String,
    /// Which routing strategy made the decision (Rule or Llm)
    pub routing_strategy: RoutingStrategy,
}

impl ChatResponse {
    /// Create a new ChatResponse with guaranteed consistency between endpoint and model_name
    ///
    /// Use this constructor in production code to ensure `model_name` always matches
    /// an actual endpoint from the configuration.
    ///
    /// # Arguments
    /// * `content` - The model's response text
    /// * `endpoint` - The endpoint that generated the response (guarantees valid model_name)
    /// * `tier` - The tier used for routing (fast, balanced, deep)
    /// * `routing_strategy` - Which routing strategy was used (Rule or Llm)
    pub fn new(
        content: String,
        endpoint: &ModelEndpoint,
        tier: TargetModel,
        routing_strategy: RoutingStrategy,
    ) -> Self {
        Self {
            content,
            model_tier: tier.into(),
            model_name: endpoint.name().to_string(),
            routing_strategy,
        }
    }
}

/// POST /chat handler
pub async fn handler(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    Json(request): Json<ChatRequest>,
) -> Result<impl IntoResponse, AppError> {
    tracing::debug!(
        request_id = %request_id,
        message_length = request.message().len(),
        importance = ?request.importance(),
        task_type = ?request.task_type(),
        "Received chat request"
    );

    // No need to validate - validation happens during deserialization

    // Convert to metadata for routing
    let metadata = request.to_metadata();

    // Use hybrid router to determine target tier
    // Tries rule-based first, falls back to LLM router if needed
    let decision = state.router().route(request.message(), &metadata).await?;

    tracing::info!(
        request_id = %request_id,
        target_tier = ?decision.target,
        routing_strategy = ?decision.strategy,
        token_estimate = metadata.token_estimate,
        "Routing decision made"
    );

    // ═══════════════════════════════════════════════════════════════════════════════
    // RETRY STRATEGY: Dual-Level Failure Tracking
    // ═══════════════════════════════════════════════════════════════════════════════
    //
    // This handler implements a sophisticated retry mechanism with two levels of failure tracking:
    //
    // 1️⃣  REQUEST-SCOPED EXCLUSION (Immediate, This Request Only)
    //    - Failed endpoints are added to `failed_endpoints` exclusion set
    //    - Prevents retrying the same endpoint within THIS single request
    //    - Guarantees each retry attempt uses a different endpoint
    //    - Clears after request completes (doesn't affect future requests)
    //
    // 2️⃣  GLOBAL HEALTH TRACKING (Persistent, Across All Requests)
    //    - Endpoints marked unhealthy after 3 consecutive failures across ALL requests
    //    - Unhealthy endpoints excluded from selection for ANY request
    //    - Background health checks (every 30s) probe unhealthy endpoints for recovery
    //    - Immediate recovery on successful request (mark_success resets failure count)
    //
    // WHY BOTH?
    // - Request-scoped exclusion ensures no wasted retries on known-bad endpoints in THIS request
    // - Global health tracking prevents all requests from hitting persistently failing endpoints
    // - Without request-scoped: Could retry the same failed endpoint on attempts 1, 2, 3
    // - Without global health: Every request would independently discover failing endpoints
    //
    // RETRY FLOW:
    // 1. Select endpoint (filtered by health status + request exclusion + priority + weight)
    // 2. Attempt query with timeout
    // 3. On success: mark_success() → return response to user
    // 4. On failure: mark_failure() + add to exclusion set → try next endpoint
    // 5. After MAX_RETRIES attempts: return error to user
    //
    // EXAMPLE WITH 2 ENDPOINTS (fast-1, fast-2) WHERE fast-1 IS DOWN:
    // - Attempt 1: Select fast-1 (50% chance), fail → add to exclusion, mark_failure (1/3)
    // - Attempt 2: Select fast-2 (100% chance, fast-1 excluded), succeed → return response
    // - If fast-1 fails 2 more times across future requests → marked unhealthy globally
    // - All future requests will only see fast-2 until background health check recovers fast-1
    //
    // ═══════════════════════════════════════════════════════════════════════════════
    const MAX_RETRIES: usize = 3;
    let mut last_error = None;
    let mut failed_endpoints = ExclusionSet::new();

    for attempt in 1..=MAX_RETRIES {
        // Select endpoint from target tier (with health filtering + priority + exclusion)
        let endpoint = match state
            .selector()
            .select(decision.target, &failed_endpoints)
            .await
        {
            Some(ep) => ep.clone(),
            None => {
                let total_configured = state.selector().endpoint_count(decision.target);
                let excluded_count = failed_endpoints.len();

                tracing::error!(
                    request_id = %request_id,
                    tier = ?decision.target,
                    attempt = attempt,
                    max_retries = MAX_RETRIES,
                    total_configured_endpoints = total_configured,
                    failed_endpoints_count = excluded_count,
                    failed_endpoints = ?failed_endpoints,
                    "No available healthy endpoints for tier. Configured: {}, Excluded: {}, \
                    This means all endpoints are either unhealthy or have failed in this request.",
                    total_configured, excluded_count
                );
                last_error = Some(AppError::RoutingFailed(format!(
                    "No available healthy endpoints for tier {:?} \
                    (configured: {}, excluded: {}, attempt {}/{})",
                    decision.target, total_configured, excluded_count, attempt, MAX_RETRIES
                )));
                continue; // Try again (may have different healthy endpoints)
            }
        };

        tracing::debug!(
            request_id = %request_id,
            endpoint_name = %endpoint.name(),
            endpoint_url = %endpoint.base_url(),
            attempt = attempt,
            max_retries = MAX_RETRIES,
            "Attempting model query"
        );

        // Try to query this endpoint
        match try_query_model(
            &endpoint,
            &request,
            state.config().server.request_timeout_seconds,
            request_id,
            attempt,
            MAX_RETRIES,
        )
        .await
        {
            Ok(response_text) => {
                // Success! Mark endpoint as healthy to enable immediate recovery
                //
                // INVARIANT CHECK: mark_success should never fail in normal operation (endpoint names come
                // from ModelSelector which only returns valid endpoints). If it fails, this indicates
                // a serious bug (race condition, typo, or config reload during request). Propagate the
                // error to expose the bug immediately rather than silently continuing.
                state
                    .selector()
                    .health_checker()
                    .mark_success(endpoint.name())
                    .await
                    .map_err(|e| {
                        use crate::models::health::HealthError;
                        match e {
                            HealthError::UnknownEndpoint(ref name) => {
                                tracing::error!(
                                    request_id = %request_id,
                                    endpoint_name = %endpoint.name(),
                                    unknown_name = %name,
                                    selected_tier = ?decision.target,
                                    attempt = attempt,
                                    "INVARIANT VIOLATION: mark_success called with unknown endpoint name. \
                                    Endpoint names come from ModelSelector which only returns valid endpoints. \
                                    This indicates a serious bug (race condition, naming mismatch, or config \
                                    reload during request). Failing request to expose issue."
                                );
                            }
                            HealthError::HttpClientCreationFailed(ref msg) => {
                                tracing::error!(
                                    request_id = %request_id,
                                    endpoint_name = %endpoint.name(),
                                    error = %msg,
                                    selected_tier = ?decision.target,
                                    attempt = attempt,
                                    "SYSTEMIC ERROR: HTTP client creation failed during health tracking. \
                                    This indicates a systemic issue (TLS configuration, resource exhaustion) \
                                    affecting ALL endpoints, not an individual endpoint problem. \
                                    Failing request to expose issue."
                                );
                            }
                        }
                        AppError::HealthCheckFailed {
                            endpoint: endpoint.name().to_string(),
                            reason: format!("mark_success failed: {}. This should not happen.", e),
                        }
                    })?;

                tracing::info!(
                    request_id = %request_id,
                    endpoint_name = %endpoint.name(),
                    response_length = response_text.len(),
                    model_tier = ?decision.target,
                    attempt = attempt,
                    "Request completed successfully"
                );

                let response =
                    ChatResponse::new(response_text, &endpoint, decision.target, decision.strategy);

                return Ok(Json(response));
            }
            Err(e) => {
                // Failure - use two separate exclusion mechanisms:
                // 1. Request-scoped exclusion (immediate, this request only)
                // 2. Global health tracking (after 3 consecutive failures across all requests)
                tracing::warn!(
                    request_id = %request_id,
                    endpoint_name = %endpoint.name(),
                    attempt = attempt,
                    max_retries = MAX_RETRIES,
                    error = %e,
                    "Endpoint query failed, excluding from retries and marking for health tracking"
                );

                // Mark this endpoint as failed for GLOBAL health tracking.
                // After 3 consecutive failures (across all requests), endpoint becomes unhealthy
                // and won't be selected by ANY request until it recovers.
                //
                // INVARIANT CHECK: mark_failure should never fail in normal operation (endpoint names come
                // from ModelSelector which only returns valid endpoints). If it fails, this indicates
                // a serious bug (race condition, typo, or config reload during request). Propagate the
                // error to expose the bug immediately rather than silently continuing.
                state
                    .selector()
                    .health_checker()
                    .mark_failure(endpoint.name())
                    .await
                    .map_err(|e| {
                        use crate::models::health::HealthError;
                        match e {
                            HealthError::UnknownEndpoint(ref name) => {
                                tracing::error!(
                                    request_id = %request_id,
                                    endpoint_name = %endpoint.name(),
                                    unknown_name = %name,
                                    selected_tier = ?decision.target,
                                    attempt = attempt,
                                    "INVARIANT VIOLATION: mark_failure called with unknown endpoint name. \
                                    Endpoint won't be marked unhealthy and will continue receiving traffic. \
                                    Endpoint names come from ModelSelector which only returns valid endpoints. \
                                    This indicates a serious bug (race condition or naming mismatch). \
                                    Failing request to expose issue."
                                );
                            }
                            HealthError::HttpClientCreationFailed(ref msg) => {
                                tracing::error!(
                                    request_id = %request_id,
                                    endpoint_name = %endpoint.name(),
                                    error = %msg,
                                    selected_tier = ?decision.target,
                                    attempt = attempt,
                                    "SYSTEMIC ERROR: HTTP client creation failed during health tracking. \
                                    This indicates a systemic issue (TLS configuration, resource exhaustion) \
                                    affecting ALL endpoints, not an individual endpoint problem. \
                                    Failing request to expose issue."
                                );
                            }
                        }
                        AppError::HealthCheckFailed {
                            endpoint: endpoint.name().to_string(),
                            reason: format!("mark_failure failed: {}. This should not happen.", e),
                        }
                    })?;

                // Exclude this endpoint from subsequent retry attempts in THIS REQUEST ONLY.
                // This is request-scoped exclusion - prevents retrying the same endpoint
                // within a single request, even if it hasn't reached 3 global failures yet.
                failed_endpoints.insert(EndpointName::from(&endpoint));

                last_error = Some(e);

                // Continue to next attempt (will select different endpoint due to exclusion)
            }
        }
    }

    // All retries exhausted
    tracing::error!(
        request_id = %request_id,
        tier = ?decision.target,
        max_retries = MAX_RETRIES,
        "All retry attempts exhausted"
    );

    Err(last_error.unwrap_or_else(|| {
        AppError::Internal(format!("All {} retry attempts exhausted", MAX_RETRIES))
    }))
}

/// Helper function to attempt querying a single endpoint
///
/// Extracted to support retry logic - attempts to query the endpoint
/// and returns the response text or an error.
async fn try_query_model(
    endpoint: &ModelEndpoint,
    request: &ChatRequest,
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
            AppError::ModelQueryFailed {
                endpoint: endpoint.base_url().to_string(),
                reason: format!("Failed to configure AgentOptions: {}", e),
            }
        })?;

    tracing::debug!(
        request_id = %request_id,
        endpoint_name = %endpoint.name(),
        message_length = request.message().len(),
        timeout_seconds = timeout_seconds,
        "Starting model query"
    );

    // Enforce request timeout - wraps the ENTIRE operation: connection establishment,
    // query initiation, and streaming all response chunks. If any part exceeds the timeout,
    // the request fails and is eligible for retry with a different endpoint.
    let timeout_duration = Duration::from_secs(timeout_seconds);

    use futures::StreamExt;
    let timeout_result = tokio::time::timeout(
        timeout_duration,
        async {
            // Query model and get stream
            let mut stream = open_agent::query(request.message(), &options)
                .await
                .map_err(|e| {
                    tracing::error!(
                        request_id = %request_id,
                        endpoint_name = %endpoint.name(),
                        error = %e,
                        "Failed to query model"
                    );
                    AppError::ModelQueryFailed {
                        endpoint: endpoint.base_url().to_string(),
                        reason: format!("{}", e),
                    }
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
                                    "Received non-text content block, skipping (not yet supported through Phase 2c)"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        // IMPORTANT: Partial response handling
                        // When a stream error occurs (network interruption, endpoint crash, etc.),
                        // we discard the partial response and return an error. The retry logic
                        // will attempt a different endpoint with a fresh request.
                        // This ensures users never receive incomplete/corrupted responses.
                        // See tests/retry_logic.rs for detailed documentation.
                        tracing::error!(
                            request_id = %request_id,
                            endpoint_name = %endpoint.name(),
                            endpoint_url = %endpoint.base_url(),
                            error = %e,
                            block_count = block_count,
                            partial_response_length = response_text.len(),
                            "Stream error after {} blocks ({} chars received). \
                            Discarding partial response and triggering retry. \
                            This could indicate network interruption (if blocks > 0) or \
                            connection failure (if blocks = 0)",
                            block_count, response_text.len()
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
        }
    )
    .await;

    // Handle timeout result with explicit match for clarity
    let response_text = match timeout_result {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => return Err(e),
        Err(_elapsed) => {
            tracing::error!(
                request_id = %request_id,
                endpoint_name = %endpoint.name(),
                endpoint_url = %endpoint.base_url(),
                timeout_seconds = timeout_seconds,
                message_length = request.message().len(),
                task_type = ?request.task_type(),
                importance = ?request.importance(),
                attempt = attempt,
                max_retries = max_retries,
                "Request timed out (including streaming). Endpoint: {} - \
                consider increasing timeout or check endpoint connectivity (attempt {}/{})",
                endpoint.base_url(), attempt, max_retries
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_request_deserializes() {
        let json = r#"{"message": "Hello!"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");

        assert_eq!(req.message(), "Hello!");
        assert_eq!(req.importance(), Importance::Normal); // default
        assert_eq!(req.task_type(), TaskType::QuestionAnswer); // default
    }

    #[test]
    fn test_chat_request_with_importance() {
        let json = r#"{"message": "Urgent!", "importance": "high"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");

        assert_eq!(req.message(), "Urgent!");
        assert_eq!(req.importance(), Importance::High);
    }

    #[test]
    fn test_chat_request_with_task_type() {
        let json = r#"{"message": "Write code", "task_type": "code"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");

        assert_eq!(req.task_type(), TaskType::Code);
    }

    #[test]
    fn test_chat_request_rejects_empty_message() {
        let json = r#"{"message": ""}"#;
        let result = serde_json::from_str::<ChatRequest>(json);

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("empty") || err_msg.contains("whitespace"),
            "error message should mention empty or whitespace, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_chat_request_rejects_whitespace_only_message() {
        let json = r#"{"message": "   \n\t  "}"#;
        let result = serde_json::from_str::<ChatRequest>(json);

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("empty") || err_msg.contains("whitespace"),
            "error message should mention empty or whitespace, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_chat_request_rejects_message_too_long() {
        let long_message = "a".repeat(100_001); // Exceeds MAX_MESSAGE_LENGTH (characters)
        let json = format!(r#"{{"message": "{}"}}"#, long_message);
        let result = serde_json::from_str::<ChatRequest>(&json);

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeds maximum length") || err_msg.contains("100000"),
            "error message should mention exceeds maximum length, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_chat_request_accepts_emoji_at_char_limit() {
        // Emoji are 4 bytes each in UTF-8, but should count as 1 character
        // 100,000 emojis = 400,000 bytes but only 100,000 characters
        let emoji_message = "👋".repeat(100_000);
        let json = format!(r#"{{"message": "{}"}}"#, emoji_message);
        let result = serde_json::from_str::<ChatRequest>(&json);

        assert!(
            result.is_ok(),
            "100K emoji chars (400K bytes) should be accepted. Error: {:?}",
            result.err()
        );
        let req = result.unwrap();
        assert_eq!(req.message().chars().count(), 100_000);
    }

    #[test]
    fn test_chat_request_rejects_emoji_over_char_limit() {
        // 100,001 emojis = 400,004 bytes but 100,001 characters
        let emoji_message = "👋".repeat(100_001);
        let json = format!(r#"{{"message": "{}"}}"#, emoji_message);
        let result = serde_json::from_str::<ChatRequest>(&json);

        assert!(
            result.is_err(),
            "100,001 emoji chars should be rejected regardless of byte count"
        );
    }

    #[test]
    fn test_chat_request_accepts_cjk_at_char_limit() {
        // CJK characters are 3 bytes each in UTF-8, but should count as 1 character
        // 100,000 Chinese chars = 300,000 bytes but only 100,000 characters
        let cjk_message = "你".repeat(100_000);
        let json = format!(r#"{{"message": "{}"}}"#, cjk_message);
        let result = serde_json::from_str::<ChatRequest>(&json);

        assert!(
            result.is_ok(),
            "100K CJK chars (300K bytes) should be accepted. Error: {:?}",
            result.err()
        );
        let req = result.unwrap();
        assert_eq!(req.message().chars().count(), 100_000);
    }

    #[test]
    fn test_chat_request_rejects_cjk_over_char_limit() {
        // 100,001 Chinese characters = 300,003 bytes but 100,001 characters
        let cjk_message = "你".repeat(100_001);
        let json = format!(r#"{{"message": "{}"}}"#, cjk_message);
        let result = serde_json::from_str::<ChatRequest>(&json);

        assert!(
            result.is_err(),
            "100,001 CJK chars should be rejected regardless of byte count"
        );
    }

    #[test]
    fn test_chat_request_accepts_valid_message() {
        let json = r#"{"message": "Hello, world!"}"#;
        let result = serde_json::from_str::<ChatRequest>(json);

        assert!(result.is_ok());
        let req = result.unwrap();
        assert_eq!(req.message(), "Hello, world!");
    }

    #[test]
    fn test_chat_request_to_metadata() {
        let json =
            r#"{"message": "What is 2+2?", "importance": "low", "task_type": "casual_chat"}"#;
        let req: ChatRequest = serde_json::from_str(json).expect("should deserialize");

        let meta = req.to_metadata();
        assert_eq!(meta.importance, Importance::Low);
        assert_eq!(meta.task_type, TaskType::CasualChat);
        assert!(meta.token_estimate > 0);
    }

    #[test]
    fn test_chat_response_serializes() {
        let resp = ChatResponse {
            content: "4".to_string(),
            model_tier: ModelTier::Fast,
            model_name: "fast-1".to_string(),
            routing_strategy: RoutingStrategy::Rule,
        };

        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"content\":\"4\""));
        assert!(json.contains("\"model_tier\":\"fast\""));
        assert!(json.contains("\"routing_strategy\":\"rule\""));
    }
}
