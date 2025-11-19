//! LLM-based router that uses an LLM to make intelligent routing decisions
//!
//! Uses the balanced tier (30B model) to analyze requests and choose the optimal target model.
//! Falls back when rule-based routing cannot determine the best model.

use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::models::endpoint_name::ExclusionSet;
use crate::models::selector::ModelSelector;
use crate::router::{RouteMetadata, TargetModel};
use std::sync::Arc;

/// LLM-powered router that uses a model to make routing decisions
///
/// Uses the balanced tier (30B) to analyze requests and choose optimal target.
/// Provides intelligent fallback when rule-based routing is ambiguous.
pub struct LlmBasedRouter {
    selector: Arc<ModelSelector>,
}

impl LlmBasedRouter {
    /// Create a new LLM-based router
    pub fn new(_config: Arc<Config>, selector: Arc<ModelSelector>) -> Self {
        Self { selector }
    }

    /// Route request using LLM analysis
    ///
    /// Queries the balanced tier model with routing prompt and metadata,
    /// parses the response to determine the optimal target model.
    pub async fn route(&self, user_prompt: &str, meta: &RouteMetadata) -> AppResult<TargetModel> {
        // Build router prompt
        let router_prompt = Self::build_router_prompt(user_prompt, meta);

        tracing::debug!(
            prompt_length = router_prompt.len(),
            user_prompt_length = user_prompt.len(),
            "Built router prompt for LLM analysis"
        );

        // Select endpoint from balanced tier (for routing decision)
        let exclusions = ExclusionSet::new();
        let endpoint = self
            .selector
            .select(TargetModel::Balanced, &exclusions)
            .await
            .ok_or_else(|| {
                AppError::Internal(
                    "No healthy endpoints available in balanced tier for routing".to_string(),
                )
            })?;

        tracing::debug!(
            endpoint_name = %endpoint.name(),
            endpoint_url = %endpoint.base_url(),
            "Selected balanced tier endpoint for routing decision"
        );

        // Build AgentOptions from endpoint
        let options = open_agent::AgentOptions::builder()
            .model(endpoint.name())
            .base_url(endpoint.base_url())
            .max_tokens(endpoint.max_tokens() as u32)
            .temperature(endpoint.temperature() as f32)
            .build()
            .map_err(|e| AppError::ModelQueryFailed {
                endpoint: endpoint.base_url().to_string(),
                reason: format!("Failed to build AgentOptions: {}", e),
            })?;

        // Query the router model
        use futures::StreamExt;
        let mut stream = open_agent::query(&router_prompt, &options)
            .await
            .map_err(|e| AppError::ModelQueryFailed {
                endpoint: endpoint.base_url().to_string(),
                reason: format!("Router query failed: {}", e),
            })?;

        // Collect response from stream
        let mut response_text = String::new();
        while let Some(result) = stream.next().await {
            match result {
                Ok(block) => {
                    use open_agent::ContentBlock;
                    if let ContentBlock::Text(text_block) = block {
                        response_text.push_str(&text_block.text);
                    }
                }
                Err(e) => {
                    return Err(AppError::ModelQueryFailed {
                        endpoint: endpoint.base_url().to_string(),
                        reason: format!("Stream error: {}", e),
                    });
                }
            }
        }

        tracing::debug!(
            response_length = response_text.len(),
            response = %response_text,
            "Received router decision from LLM"
        );

        // Parse routing decision
        Self::parse_routing_decision(&response_text)
    }

    /// Build router prompt from user request + metadata
    ///
    /// Creates a structured prompt that asks the LLM to choose between
    /// FAST, BALANCED, or DEEP based on the user's request and metadata.
    fn build_router_prompt(user_prompt: &str, meta: &RouteMetadata) -> String {
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
             Respond with ONLY one of: FAST, BALANCED, DEEP",
            user_prompt, meta.token_estimate, meta.importance, meta.task_type
        )
    }

    /// Parse LLM response to extract routing decision
    ///
    /// Uses fuzzy matching to extract FAST, BALANCED, or DEEP from the response.
    /// Defaults to BALANCED on ambiguous responses.
    fn parse_routing_decision(response: &str) -> AppResult<TargetModel> {
        let normalized = response.trim().to_uppercase();

        // Check for FAST first (highest priority in parsing order)
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

        // Default to Balanced on ambiguous or empty responses
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
    fn test_parse_routing_decision_empty_defaults_to_balanced() {
        let response = "";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Balanced);
    }

    #[test]
    fn test_parse_routing_decision_whitespace_defaults_to_balanced() {
        let response = "   \n\t  ";
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), TargetModel::Balanced);
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
}
