//! Routing logic for Octoroute
//!
//! Provides different routing strategies to select the optimal model for a request.

pub mod hybrid;
pub mod llm_based;
pub mod rule_based;

pub use hybrid::HybridRouter;
pub use llm_based::{LlmBasedRouter, LlmRouter};
pub use rule_based::RuleBasedRouter;

use crate::error::AppResult;
use serde::{Deserialize, Serialize};

/// Target model selection (generic tiers)
///
/// Maps to config.toml: models.fast, models.balanced, models.deep
/// Model-specific details (size, name, endpoint) are in configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetModel {
    Fast,
    Balanced,
    Deep,
}

/// Routing strategy used to make a routing decision
///
/// Provides compile-time type safety for routing strategy tracking
/// instead of using raw strings which are error-prone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoutingStrategy {
    /// Rule-based routing (fast path, deterministic)
    Rule,
    /// LLM-based routing (intelligent fallback for ambiguous cases)
    Llm,
}

impl RoutingStrategy {
    /// Convert to string representation for logging and serialization
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Llm => "llm",
        }
    }
}

/// Result of a routing decision
///
/// Combines the target model tier with the strategy that was used
/// to make the decision. Provides better type safety and clarity
/// than returning a tuple.
///
/// Fields are private to enable future validation logic and maintain
/// encapsulation. Use accessor methods `target()` and `strategy()` to
/// read the values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingDecision {
    /// Which model tier to use
    target: TargetModel,
    /// Which routing strategy made the decision
    strategy: RoutingStrategy,
}

impl RoutingDecision {
    /// Create a new routing decision
    pub fn new(target: TargetModel, strategy: RoutingStrategy) -> Self {
        Self { target, strategy }
    }

    /// Get the target model tier for this routing decision
    pub fn target(&self) -> TargetModel {
        self.target
    }

    /// Get the routing strategy that made this decision
    pub fn strategy(&self) -> RoutingStrategy {
        self.strategy
    }
}

/// Request importance level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Importance {
    Low,
    #[default]
    Normal,
    High,
}

/// Task type classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    CasualChat,
    Code,
    CreativeWriting,
    DeepAnalysis,
    DocumentSummary,
    #[default]
    QuestionAnswer,
}

/// Metadata extracted from a request to inform routing decisions
#[derive(Debug, Clone, Copy)]
pub struct RouteMetadata {
    /// Estimated token count for the request
    pub token_estimate: usize,
    /// User-specified importance level
    pub importance: Importance,
    /// Task type classification
    pub task_type: TaskType,
}

impl RouteMetadata {
    /// Create a new RouteMetadata with defaults
    pub fn new(token_estimate: usize) -> Self {
        Self {
            token_estimate,
            importance: Importance::default(),
            task_type: TaskType::default(),
        }
    }

    /// Set the importance level
    pub fn with_importance(mut self, importance: Importance) -> Self {
        self.importance = importance;
        self
    }

    /// Set the task type
    pub fn with_task_type(mut self, task_type: TaskType) -> Self {
        self.task_type = task_type;
        self
    }

    /// Estimate token count from a prompt string (simple heuristic: chars / 4)
    pub fn estimate_tokens(prompt: &str) -> usize {
        prompt.chars().count() / 4
    }
}

/// Router type enum supporting different routing strategies
///
/// Enables clean separation of router types and allows conditional construction
/// based on configuration. Each variant wraps its corresponding router implementation.
///
/// # Configuration-Driven Construction
/// The router type is determined by `config.routing.strategy`:
/// - `Rule`: Only rule-based routing (no LLM routing, no balanced tier required)
/// - `Llm`: Only LLM-based routing (requires balanced tier configured)
/// - `Hybrid`: Rule-based with LLM fallback (requires balanced tier configured)
///
/// This design allows deployments to opt-out of LLM routing (and its balanced tier requirement)
/// by setting `strategy = "rule"` in configuration.
pub enum Router {
    /// Rule-based router (deterministic, fast, no LLM required)
    Rule(RuleBasedRouter),
    /// LLM-based router (intelligent, requires balanced tier)
    Llm(LlmBasedRouter),
    /// Hybrid router (rule-based with LLM fallback, requires balanced tier)
    Hybrid(HybridRouter),
}

impl Router {
    /// Route a request using the configured strategy
    ///
    /// Delegates to the appropriate router implementation based on the variant.
    /// All routers return a RoutingDecision containing the target tier and
    /// the strategy that made the decision.
    ///
    /// # Arguments
    /// * `user_prompt` - The user's prompt/message
    /// * `meta` - Request metadata (token estimate, importance, task type)
    /// * `selector` - Model selector (needed for rule-based default tier fallback)
    ///
    /// # Errors
    /// Returns an error if:
    /// - LLM routing fails (network error, no healthy balanced endpoints, etc.)
    /// - Rule routing with no match and no default tier available
    pub async fn route(
        &self,
        user_prompt: &str,
        meta: &RouteMetadata,
        selector: &crate::models::ModelSelector,
    ) -> AppResult<RoutingDecision> {
        match self {
            Router::Rule(r) => {
                // Rule router returns Option - if None, use default tier fallback
                match r.route(user_prompt, meta, selector).await? {
                    Some(decision) => Ok(decision),
                    None => {
                        // No rule matched - use default tier for rule-only mode
                        let default_target = selector.default_tier().ok_or_else(|| {
                            crate::error::AppError::Config(
                                "No routing rule matched and no endpoints configured for default fallback"
                                    .to_string(),
                            )
                        })?;

                        // Verify default tier has healthy endpoints
                        let exclusion_set = crate::models::ExclusionSet::new();
                        if selector
                            .select(default_target, &exclusion_set)
                            .await
                            .is_none()
                        {
                            return Err(crate::error::AppError::RoutingFailed(format!(
                                "No rule matched and default tier {:?} has no healthy endpoints available",
                                default_target
                            )));
                        }

                        tracing::info!(
                            default_tier = ?default_target,
                            token_estimate = meta.token_estimate,
                            importance = ?meta.importance,
                            task_type = ?meta.task_type,
                            "No rule matched, using default tier (rule-only mode)"
                        );

                        Ok(RoutingDecision::new(default_target, RoutingStrategy::Rule))
                    }
                }
            }
            Router::Llm(r) => r.route(user_prompt, meta).await,
            Router::Hybrid(r) => r.route(user_prompt, meta).await,
        }
    }
}

/// Parse router_model string to TargetModel enum
///
/// Validates that the router_model configuration value is valid and converts
/// to the appropriate TargetModel enum variant. This helper eliminates code
/// duplication across handlers and router initialization.
///
/// # Arguments
/// * `router_model` - The router_model string from configuration (must be "fast", "balanced", or "deep")
///
/// # Returns
/// * `Ok(TargetModel)` if valid
/// * `Err(AppError::Config)` if invalid with actionable error message
///
/// # Examples
/// ```
/// use octoroute::router::{parse_router_tier, TargetModel};
///
/// let tier = parse_router_tier("balanced").unwrap();
/// assert_eq!(tier, TargetModel::Balanced);
///
/// assert!(parse_router_tier("FAST").is_err()); // case sensitive
/// ```
pub fn parse_router_tier(router_model: &str) -> AppResult<TargetModel> {
    match router_model {
        "fast" => Ok(TargetModel::Fast),
        "balanced" => Ok(TargetModel::Balanced),
        "deep" => Ok(TargetModel::Deep),
        invalid => Err(crate::error::AppError::Config(format!(
            "Invalid router_model '{}'. Expected 'fast', 'balanced', or 'deep'. \
             Valid values are case-sensitive (lowercase only). \
             Common mistakes: capitalization (e.g., 'FAST'), typos (e.g., 'balance').",
            invalid
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_router_tier_valid_values() {
        assert!(matches!(parse_router_tier("fast"), Ok(TargetModel::Fast)));
        assert!(matches!(
            parse_router_tier("balanced"),
            Ok(TargetModel::Balanced)
        ));
        assert!(matches!(parse_router_tier("deep"), Ok(TargetModel::Deep)));
    }

    #[test]
    fn test_parse_router_tier_invalid_values() {
        let invalid_cases = vec!["FAST", "Fast", "invalid", "", "medium", "Balanced", "DEEP"];

        for invalid in invalid_cases {
            let result = parse_router_tier(invalid);
            assert!(
                result.is_err(),
                "Should reject '{}' as invalid router_model",
                invalid
            );

            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains(invalid) || err_msg.contains("router_model"),
                "Error should reference the invalid value or field name, got: {}",
                err_msg
            );
        }
    }

    #[test]
    fn test_parse_router_tier_error_message_quality() {
        let result = parse_router_tier("FAST");
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();

        // Error should be actionable
        assert!(err_msg.contains("'FAST'"), "Should quote the invalid value");
        assert!(
            err_msg.contains("fast") || err_msg.contains("balanced") || err_msg.contains("deep"),
            "Should list valid values, got: {}",
            err_msg
        );
        assert!(
            err_msg.contains("case-sensitive") || err_msg.contains("lowercase"),
            "Should mention case sensitivity, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_target_model_enum_values() {
        let fast = TargetModel::Fast;
        let balanced = TargetModel::Balanced;
        let deep = TargetModel::Deep;

        assert_eq!(fast, TargetModel::Fast);
        assert_eq!(balanced, TargetModel::Balanced);
        assert_eq!(deep, TargetModel::Deep);
    }

    #[test]
    fn test_importance_default() {
        assert_eq!(Importance::default(), Importance::Normal);
    }

    #[test]
    fn test_task_type_default() {
        assert_eq!(TaskType::default(), TaskType::QuestionAnswer);
    }

    #[test]
    fn test_route_metadata_new() {
        let meta = RouteMetadata::new(100);
        assert_eq!(meta.token_estimate, 100);
        assert_eq!(meta.importance, Importance::Normal);
        assert_eq!(meta.task_type, TaskType::QuestionAnswer);
    }

    #[test]
    fn test_route_metadata_builder() {
        let meta = RouteMetadata::new(200)
            .with_importance(Importance::High)
            .with_task_type(TaskType::Code);

        assert_eq!(meta.token_estimate, 200);
        assert_eq!(meta.importance, Importance::High);
        assert_eq!(meta.task_type, TaskType::Code);
    }

    #[test]
    fn test_estimate_tokens() {
        let prompt = "Hello, world!";
        let estimate = RouteMetadata::estimate_tokens(prompt);
        // "Hello, world!" = 13 chars / 4 = 3 tokens
        assert_eq!(estimate, 3);

        let long_prompt = "a".repeat(1000);
        let long_estimate = RouteMetadata::estimate_tokens(&long_prompt);
        assert_eq!(long_estimate, 250); // 1000 / 4
    }

    #[test]
    fn test_importance_serde() {
        assert_eq!(
            serde_json::from_str::<Importance>(r#""low""#).unwrap(),
            Importance::Low
        );
        assert_eq!(
            serde_json::from_str::<Importance>(r#""normal""#).unwrap(),
            Importance::Normal
        );
        assert_eq!(
            serde_json::from_str::<Importance>(r#""high""#).unwrap(),
            Importance::High
        );
    }

    #[test]
    fn test_task_type_serde() {
        assert_eq!(
            serde_json::from_str::<TaskType>(r#""casual_chat""#).unwrap(),
            TaskType::CasualChat
        );
        assert_eq!(
            serde_json::from_str::<TaskType>(r#""code""#).unwrap(),
            TaskType::Code
        );
        assert_eq!(
            serde_json::from_str::<TaskType>(r#""creative_writing""#).unwrap(),
            TaskType::CreativeWriting
        );
    }

    #[test]
    fn test_routing_strategy_as_str() {
        assert_eq!(RoutingStrategy::Rule.as_str(), "rule");
        assert_eq!(RoutingStrategy::Llm.as_str(), "llm");
    }

    #[test]
    fn test_routing_strategy_serde() {
        // Test deserialization
        assert_eq!(
            serde_json::from_str::<RoutingStrategy>(r#""rule""#).unwrap(),
            RoutingStrategy::Rule
        );
        assert_eq!(
            serde_json::from_str::<RoutingStrategy>(r#""llm""#).unwrap(),
            RoutingStrategy::Llm
        );

        // Test serialization
        assert_eq!(
            serde_json::to_string(&RoutingStrategy::Rule).unwrap(),
            r#""rule""#
        );
        assert_eq!(
            serde_json::to_string(&RoutingStrategy::Llm).unwrap(),
            r#""llm""#
        );
    }

    #[test]
    fn test_routing_decision_new() {
        let decision = RoutingDecision::new(TargetModel::Fast, RoutingStrategy::Rule);
        assert_eq!(decision.target(), TargetModel::Fast);
        assert_eq!(decision.strategy(), RoutingStrategy::Rule);
    }

    #[test]
    fn test_routing_decision_accessors() {
        // ISSUE #1a: Test accessor methods (to support private fields)
        //
        // This test verifies that RoutingDecision provides proper accessor methods
        // for its fields, enabling encapsulation and future validation logic.

        let decision = RoutingDecision::new(TargetModel::Balanced, RoutingStrategy::Llm);

        // Test target() accessor
        assert_eq!(
            decision.target(),
            TargetModel::Balanced,
            "target() should return the target model"
        );

        // Test strategy() accessor
        assert_eq!(
            decision.strategy(),
            RoutingStrategy::Llm,
            "strategy() should return the routing strategy"
        );
    }

    #[test]
    fn test_routing_decision_accessors_all_variants() {
        // ISSUE #1a: Verify accessors work for all enum variants
        //
        // Ensures accessor methods correctly return all possible values

        let test_cases = vec![
            (TargetModel::Fast, RoutingStrategy::Rule),
            (TargetModel::Balanced, RoutingStrategy::Rule),
            (TargetModel::Deep, RoutingStrategy::Rule),
            (TargetModel::Fast, RoutingStrategy::Llm),
            (TargetModel::Balanced, RoutingStrategy::Llm),
            (TargetModel::Deep, RoutingStrategy::Llm),
        ];

        for (target, strategy) in test_cases {
            let decision = RoutingDecision::new(target, strategy);
            assert_eq!(decision.target(), target);
            assert_eq!(decision.strategy(), strategy);
        }
    }

    #[test]
    fn test_routing_decision_equality() {
        let decision1 = RoutingDecision::new(TargetModel::Balanced, RoutingStrategy::Llm);
        let decision2 = RoutingDecision::new(TargetModel::Balanced, RoutingStrategy::Llm);
        let decision3 = RoutingDecision::new(TargetModel::Fast, RoutingStrategy::Rule);

        assert_eq!(decision1, decision2);
        assert_ne!(decision1, decision3);
    }
}
