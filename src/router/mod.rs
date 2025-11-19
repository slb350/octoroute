//! Routing logic for Octoroute
//!
//! Provides different routing strategies to select the optimal model for a request.

pub mod hybrid;
pub mod llm_based;
pub mod rule_based;

pub use hybrid::HybridRouter;
pub use llm_based::LlmBasedRouter;
pub use rule_based::RuleBasedRouter;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingDecision {
    /// Which model tier to use
    pub target: TargetModel,
    /// Which routing strategy made the decision
    pub strategy: RoutingStrategy,
}

impl RoutingDecision {
    /// Create a new routing decision
    pub fn new(target: TargetModel, strategy: RoutingStrategy) -> Self {
        Self { target, strategy }
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
#[derive(Debug, Clone)]
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(decision.target, TargetModel::Fast);
        assert_eq!(decision.strategy, RoutingStrategy::Rule);
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
