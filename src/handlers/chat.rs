//! Chat endpoint handler
//!
//! Handles POST /chat requests with intelligent model routing.

use crate::config::ModelEndpoint;
use crate::error::{AppError, AppResult};
use crate::handlers::AppState;
use crate::models::{EndpointName, ExclusionSet};
use crate::router::{Importance, RouteMetadata, TaskType};
use axum::{Json, extract::State, response::IntoResponse};
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

        // Validate message length
        if raw.message.len() > MAX_MESSAGE_LENGTH {
            return Err(serde::de::Error::custom(format!(
                "message exceeds maximum length of {} characters (got {})",
                MAX_MESSAGE_LENGTH,
                raw.message.len()
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
}

/// POST /chat handler
pub async fn handler(
    State(state): State<AppState>,
    Json(request): Json<ChatRequest>,
) -> Result<impl IntoResponse, AppError> {
    tracing::debug!(
        message_length = request.message().len(),
        importance = ?request.importance(),
        task_type = ?request.task_type(),
        "Received chat request"
    );

    // No need to validate - validation happens during deserialization

    // Convert to metadata for routing
    let metadata = request.to_metadata();

    // Use rule-based router to determine target tier, with fallback to default
    let target = state.router().route(&metadata).unwrap_or_else(|| {
        // When no rule matches, fall back to balanced tier as a sensible default
        // This handles common cases like simple questions that don't match specific rules
        tracing::warn!(
            task_type = ?metadata.task_type,
            importance = ?metadata.importance,
            token_estimate = metadata.token_estimate,
            "No routing rule matched, falling back to Balanced tier"
        );
        use crate::router::TargetModel;
        TargetModel::Balanced
    });

    tracing::info!(
        target_tier = ?target,
        token_estimate = metadata.token_estimate,
        "Routing decision made"
    );

    // Retry logic: Attempt up to MAX_RETRIES times with different endpoints
    // Track endpoints that have failed in THIS request to avoid retrying them
    const MAX_RETRIES: usize = 3;
    let mut last_error = None;
    let mut failed_endpoints = ExclusionSet::new();

    for attempt in 1..=MAX_RETRIES {
        // Select endpoint from target tier (with health filtering + priority + exclusion)
        let endpoint = match state.selector().select(target, &failed_endpoints).await {
            Some(ep) => ep.clone(),
            None => {
                let total_configured = state.selector().endpoint_count(target);
                let excluded_count = failed_endpoints.len();

                tracing::error!(
                    tier = ?target,
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
                    target, total_configured, excluded_count, attempt, MAX_RETRIES
                )));
                continue; // Try again (may have different healthy endpoints)
            }
        };

        tracing::debug!(
            endpoint_name = %endpoint.name,
            endpoint_url = %endpoint.base_url,
            attempt = attempt,
            max_retries = MAX_RETRIES,
            "Attempting model query"
        );

        // Try to query this endpoint
        match try_query_model(
            &endpoint,
            &request,
            state.config().server.request_timeout_seconds,
            attempt,
            MAX_RETRIES,
        )
        .await
        {
            Ok(response_text) => {
                // Success! Mark endpoint as healthy to enable immediate recovery
                if let Err(e) = state
                    .selector()
                    .health_checker()
                    .mark_success(&endpoint.name)
                    .await
                {
                    tracing::error!(
                        endpoint_name = %endpoint.name,
                        error = %e,
                        "Failed to mark endpoint as healthy - this should never happen"
                    );
                }

                tracing::info!(
                    endpoint_name = %endpoint.name,
                    response_length = response_text.len(),
                    model_tier = ?target,
                    attempt = attempt,
                    "Request completed successfully"
                );

                let response = ChatResponse {
                    content: response_text,
                    model_tier: target.into(),
                    model_name: endpoint.name.clone(),
                };

                return Ok(Json(response));
            }
            Err(e) => {
                // Failure - use two separate exclusion mechanisms:
                // 1. Request-scoped exclusion (immediate, this request only)
                // 2. Global health tracking (after 3 consecutive failures across all requests)
                tracing::warn!(
                    endpoint_name = %endpoint.name,
                    attempt = attempt,
                    max_retries = MAX_RETRIES,
                    error = %e,
                    "Endpoint query failed, excluding from retries and marking for health tracking"
                );

                // Mark this endpoint as failed for GLOBAL health tracking.
                // After 3 consecutive failures (across all requests), endpoint becomes unhealthy
                // and won't be selected by ANY request until it recovers.
                if let Err(e) = state
                    .selector()
                    .health_checker()
                    .mark_failure(&endpoint.name)
                    .await
                {
                    tracing::error!(
                        endpoint_name = %endpoint.name,
                        error = %e,
                        "Failed to mark endpoint as failed - this should never happen"
                    );
                }

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
        tier = ?target,
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
    attempt: usize,
    max_retries: usize,
) -> AppResult<String> {
    // Build AgentOptions from selected endpoint
    let options = open_agent::AgentOptions::builder()
        .model(&endpoint.name)
        .base_url(&endpoint.base_url)
        .max_tokens(endpoint.max_tokens as u32)
        .temperature(endpoint.temperature as f32)
        .build()
        .map_err(|e| {
            tracing::error!(
                endpoint_name = %endpoint.name,
                endpoint_url = %endpoint.base_url,
                max_tokens = endpoint.max_tokens,
                temperature = endpoint.temperature,
                error = %e,
                "Failed to build AgentOptions from endpoint configuration"
            );
            AppError::Internal(format!(
                "Failed to configure model endpoint '{}': {}",
                endpoint.name, e
            ))
        })?;

    tracing::debug!(
        endpoint_name = %endpoint.name,
        message_length = request.message().len(),
        timeout_seconds = timeout_seconds,
        "Starting model query"
    );

    // Enforce request timeout - wraps the ENTIRE operation: connection establishment,
    // query initiation, and streaming all response chunks. If any part exceeds the timeout,
    // the request fails and is eligible for retry with a different endpoint.
    let timeout_duration = Duration::from_secs(timeout_seconds);

    use futures::StreamExt;
    let response_text = tokio::time::timeout(
        timeout_duration,
        async {
            // Query model and get stream
            let mut stream = open_agent::query(request.message(), &options)
                .await
                .map_err(|e| {
                    tracing::error!(
                        endpoint_name = %endpoint.name,
                        error = %e,
                        "Failed to query model"
                    );
                    AppError::Internal(format!("Failed to query model: {}", e))
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
                                    endpoint_name = %endpoint.name,
                                    block_type = ?other_block,
                                    block_number = block_count,
                                    "Received non-text content block, skipping (not yet supported in Phase 2a)"
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            endpoint_name = %endpoint.name,
                            endpoint_url = %endpoint.base_url,
                            error = %e,
                            block_count = block_count,
                            partial_response_length = response_text.len(),
                            "Stream error after {} blocks ({} chars received). \
                            This could indicate network interruption (if blocks > 0) or \
                            connection failure (if blocks = 0)",
                            block_count, response_text.len()
                        );
                        return Err(AppError::Internal(format!(
                            "Stream error from {}: {} (after {} blocks, {} chars received)",
                            endpoint.base_url, e, block_count, response_text.len()
                        )));
                    }
                }
            }

            Ok::<String, AppError>(response_text)
        }
    )
    .await
    .map_err(|_| {
        tracing::error!(
            endpoint_name = %endpoint.name,
            endpoint_url = %endpoint.base_url,
            timeout_seconds = timeout_seconds,
            message_length = request.message().len(),
            task_type = ?request.task_type(),
            importance = ?request.importance(),
            attempt = attempt,
            max_retries = max_retries,
            "Request timed out (including streaming). Endpoint: {} - \
            consider increasing timeout or check endpoint connectivity (attempt {}/{})",
            endpoint.base_url, attempt, max_retries
        );
        AppError::Internal(format!(
            "Request to {} timed out after {} seconds (attempt {}/{})",
            endpoint.base_url, timeout_seconds, attempt, max_retries
        ))
    })??;

    tracing::info!(
        endpoint_name = %endpoint.name,
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
        let long_message = "a".repeat(100_001); // Exceeds MAX_MESSAGE_LENGTH
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
        };

        let json = serde_json::to_string(&resp).expect("should serialize");
        assert!(json.contains("\"content\":\"4\""));
        assert!(json.contains("\"model_tier\":\"fast\""));
    }
}
