//! LLM-based router that uses an LLM to make intelligent routing decisions
//!
//! Uses the balanced tier (30B model) to analyze requests and choose the optimal target model.
//! Falls back when rule-based routing cannot determine the best model.

use crate::error::{AppError, AppResult};
use crate::models::endpoint_name::ExclusionSet;
use crate::models::selector::ModelSelector;
use crate::router::{RouteMetadata, TargetModel};
use std::sync::Arc;

/// Maximum size for router LLM response (bytes)
///
/// Prevents unbounded memory growth if a malfunctioning LLM sends gigabytes.
/// Router responses should only be "FAST", "BALANCED", or "DEEP" (~10 bytes),
/// so 1KB is extremely generous and handles verbose responses while preventing OOM.
const MAX_ROUTER_RESPONSE: usize = 1024;

/// LLM-powered router that uses a model to make routing decisions
///
/// Uses the balanced tier (30B) to analyze requests and choose optimal target.
/// Provides intelligent fallback when rule-based routing is ambiguous.
pub struct LlmBasedRouter {
    selector: Arc<ModelSelector>,
}

impl LlmBasedRouter {
    /// Create a new LLM-based router
    pub fn new(selector: Arc<ModelSelector>) -> Self {
        Self { selector }
    }

    /// Route request using LLM analysis
    ///
    /// Queries the balanced tier model with routing prompt and metadata,
    /// parses the response to determine the optimal target model.
    /// Implements retry logic with health tracking for resilience.
    pub async fn route(&self, user_prompt: &str, meta: &RouteMetadata) -> AppResult<TargetModel> {
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
            // Select endpoint from balanced tier (with health filtering + exclusions)
            let endpoint = match self
                .selector
                .select(TargetModel::Balanced, &failed_endpoints)
                .await
            {
                Some(ep) => ep.clone(),
                None => {
                    let total_configured = self.selector.endpoint_count(TargetModel::Balanced);
                    let excluded_count = failed_endpoints.len();

                    tracing::error!(
                        tier = "Balanced",
                        attempt = attempt,
                        max_retries = MAX_ROUTER_RETRIES,
                        total_configured_endpoints = total_configured,
                        failed_endpoints_count = excluded_count,
                        failed_endpoints = ?failed_endpoints,
                        "No healthy balanced tier endpoints for routing decision. \
                        Configured: {}, Excluded: {}",
                        total_configured, excluded_count
                    );
                    last_error = Some(AppError::RoutingFailed(format!(
                        "No healthy balanced tier endpoints for routing \
                        (configured: {}, excluded: {}, attempt {}/{})",
                        total_configured, excluded_count, attempt, MAX_ROUTER_RETRIES
                    )));
                    continue;
                }
            };

            tracing::debug!(
                endpoint_name = %endpoint.name(),
                endpoint_url = %endpoint.base_url(),
                attempt = attempt,
                max_retries = MAX_ROUTER_RETRIES,
                "Selected balanced tier endpoint for routing decision"
            );

            // Try to query this endpoint
            let query_result = self
                .try_router_query(&endpoint, &router_prompt, attempt, MAX_ROUTER_RETRIES)
                .await;

            match query_result {
                Ok(target_model) => {
                    // Success! Mark endpoint healthy for immediate recovery
                    if let Err(e) = self
                        .selector
                        .health_checker()
                        .mark_success(endpoint.name())
                        .await
                    {
                        tracing::warn!(
                            endpoint_name = %endpoint.name(),
                            error = %e,
                            "Failed to mark router endpoint healthy after successful query"
                        );
                    }

                    tracing::info!(
                        endpoint_name = %endpoint.name(),
                        target_model = ?target_model,
                        attempt = attempt,
                        "Router LLM successfully determined target model"
                    );

                    return Ok(target_model);
                }
                Err(e) => {
                    // Failure - mark endpoint unhealthy and retry with different endpoint
                    tracing::warn!(
                        endpoint_name = %endpoint.name(),
                        attempt = attempt,
                        max_retries = MAX_ROUTER_RETRIES,
                        error = %e,
                        "Router query failed, marking endpoint for health tracking and retrying"
                    );

                    if let Err(health_err) = self
                        .selector
                        .health_checker()
                        .mark_failure(endpoint.name())
                        .await
                    {
                        tracing::warn!(
                            endpoint_name = %endpoint.name(),
                            error = %health_err,
                            "Failed to mark router endpoint unhealthy after failed query"
                        );
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
            tier = "Balanced",
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
                        "Failed to configure AgentOptions: {} (model={}, max_tokens={})",
                        e,
                        endpoint.name(),
                        endpoint.max_tokens()
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
                    reason: format!("Router query timeout after {} seconds", ROUTER_QUERY_TIMEOUT_SECS),
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
                            tracing::warn!(
                                endpoint_name = %endpoint.name(),
                                current_length = response_text.len(),
                                incoming_length = text_block.text.len(),
                                max_allowed = MAX_ROUTER_RESPONSE,
                                attempt = attempt,
                                "Router response exceeded size limit, truncating (attempt {}/{})",
                                attempt, max_retries
                            );
                            break; // Stop accumulating, parse what we have
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
                        reason: format!("Stream error: {}", e),
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
        let truncated_prompt = if user_prompt.len() > MAX_USER_PROMPT_CHARS {
            format!("{}... [truncated]", &user_prompt[..MAX_USER_PROMPT_CHARS])
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

    /// Parse LLM response to extract routing decision
    ///
    /// Uses fuzzy matching to extract FAST, BALANCED, or DEEP from the response.
    /// Returns an error on empty responses (indicates LLM misconfiguration or refusal).
    fn parse_routing_decision(response: &str) -> AppResult<TargetModel> {
        let normalized = response.trim().to_uppercase();

        // Check for empty response first - this indicates a problem (misconfiguration, safety filter, etc.)
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

        // Fuzzy matching: Check for each keyword in order
        // Order matters to avoid false positives with substring collisions

        // Check for FAST first to avoid false positives
        if normalized.contains("FAST") {
            return Ok(TargetModel::Fast);
        }

        // Check for BALANCED second
        if normalized.contains("BALANCED") {
            return Ok(TargetModel::Balanced);
        }

        // Check for DEEP third
        if normalized.contains("DEEP") {
            return Ok(TargetModel::Deep);
        }

        // Default to Balanced - middle-ground choice:
        // - Fast might be too weak if LLM couldn't decide (ambiguous = complex)
        // - Deep wastes compute if response was just malformed
        // - Balanced handles most tasks adequately
        tracing::warn!(
            response = %response,
            "Could not parse router response, defaulting to Balanced"
        );
        Ok(TargetModel::Balanced)
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
    fn test_parse_routing_decision_ambiguous_defaults_to_balanced() {
        let response = "I'm not sure about this one";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Balanced);
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

    #[test]
    fn test_parse_routing_decision_truncates_long_responses() {
        // Response size limit should prevent unbounded memory growth
        // Very long responses should be truncated but still parsed correctly
        let long_response = format!(
            "{}BALANCED{}",
            "x".repeat(500), // 500 chars before
            "y".repeat(500)  // 500 chars after
        );

        let result = LlmBasedRouter::parse_routing_decision(&long_response);
        assert!(
            result.is_ok(),
            "Long responses should still parse correctly"
        );
        assert_eq!(result.unwrap(), TargetModel::Balanced);
    }

    #[test]
    fn test_parse_routing_decision_handles_extreme_length() {
        // Even with megabyte-sized responses, parsing should work
        // (though the actual streaming should have truncated it)
        let extreme_response = format!("FAST{}", "x".repeat(1_000_000));

        let result = LlmBasedRouter::parse_routing_decision(&extreme_response);
        assert!(result.is_ok(), "Extreme length should not crash parser");
        assert_eq!(result.unwrap(), TargetModel::Fast);
    }
}
