//! Rule-based routing strategy
//!
//! Fast, deterministic routing using pattern matching on metadata.
//! Zero LLM overhead - all decisions are pure CPU logic.
//!
//! Routes to generic model tiers (Fast, Balanced, Deep) based on:
//! - Task type and complexity
//! - Token count estimates
//! - User-specified importance level

use super::{Importance, RouteMetadata, RoutingDecision, RoutingStrategy, TargetModel, TaskType};
use crate::error::{AppError, AppResult};
use crate::models::ModelSelector;

/// Rule-based router that uses fast pattern matching
#[derive(Debug, Clone, Default)]
pub struct RuleBasedRouter;

impl RuleBasedRouter {
    /// Create a new rule-based router
    pub fn new() -> Self {
        Self
    }

    /// Route a request based on metadata using rule-based logic
    ///
    /// # Routing Logic
    /// 1. Tries to match request metadata against predefined rules
    /// 2. If a rule matches, returns that tier with RoutingStrategy::Rule
    /// 3. If no rule matches, falls back to selector.default_tier() (highest priority tier across all tiers)
    ///
    /// # Arguments
    /// * `_user_prompt` - The user's prompt (unused in rule-based routing, but required for Router trait compatibility)
    /// * `meta` - Request metadata (token estimate, importance, task type)
    /// * `selector` - Model selector for default tier fallback
    ///
    /// # Errors
    /// Returns an error if no rules match AND no endpoints are configured at all.
    pub async fn route(
        &self,
        _user_prompt: &str,
        meta: &RouteMetadata,
        selector: &ModelSelector,
    ) -> AppResult<RoutingDecision> {
        // Try rule-based matching first
        if let Some(target) = self.evaluate_rules(meta) {
            return Ok(RoutingDecision::new(target, RoutingStrategy::Rule));
        }

        // No rule matched - fall back to default tier (highest priority across all tiers)
        let default_target = selector.default_tier().ok_or_else(|| {
            AppError::Config(
                "No routing rule matched and no endpoints configured for default fallback"
                    .to_string(),
            )
        })?;

        // Verify that the default tier has at least one healthy endpoint
        // If all endpoints are unhealthy, we should fail instead of returning a tier
        // that can't actually be used
        let exclusion_set = crate::models::ExclusionSet::new();
        if selector
            .select(default_target, &exclusion_set)
            .await
            .is_none()
        {
            return Err(AppError::RoutingFailed(format!(
                "No rule matched and default tier {:?} has no healthy endpoints available",
                default_target
            )));
        }

        tracing::info!(
            default_tier = ?default_target,
            token_estimate = meta.token_estimate,
            importance = ?meta.importance,
            task_type = ?meta.task_type,
            "No rule matched, using default tier (highest priority across all tiers)"
        );

        Ok(RoutingDecision::new(default_target, RoutingStrategy::Rule))
    }

    /// Evaluate rules against metadata
    ///
    /// Returns `Some(TargetModel)` if a rule matches, `None` otherwise.
    /// This is the internal rule evaluation logic, separated for testing.
    fn evaluate_rules(&self, meta: &RouteMetadata) -> Option<TargetModel> {
        use Importance::*;
        use TaskType::*;

        // Rule 1: Trivial/casual tasks → Fast tier
        if matches!(meta.task_type, CasualChat)
            && meta.token_estimate < 256
            && !matches!(meta.importance, High)
        {
            return Some(TargetModel::Fast);
        }

        // Rule 2: High importance or deep work → Deep tier
        // (Check this BEFORE medium-depth rule to prioritize importance)
        // (Exclude CasualChat + High as it's ambiguous → delegate to LLM)
        if (matches!(meta.importance, High) && !matches!(meta.task_type, CasualChat))
            || matches!(meta.task_type, DeepAnalysis | CreativeWriting)
        {
            return Some(TargetModel::Deep);
        }

        // Rule 3: Code generation (special case)
        if matches!(meta.task_type, Code) {
            return if meta.token_estimate > 1024 {
                Some(TargetModel::Deep)
            } else {
                Some(TargetModel::Balanced)
            };
        }

        // Rule 4: Medium-depth tasks → Balanced tier
        // (Only non-code, non-deep tasks with sufficient complexity)
        // (Minimum 200 tokens to justify balanced model)
        if meta.token_estimate >= 200
            && meta.token_estimate < 2048
            && matches!(meta.task_type, QuestionAnswer | DocumentSummary)
        {
            return Some(TargetModel::Balanced);
        }

        // No rule matched → delegate to LLM router
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::sync::Arc;

    /// Helper to create test config for rule router tests
    fn test_config() -> Arc<Config> {
        let toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
default_importance = "normal"
router_model = "balanced"

[observability]
log_level = "info"
"#;
        Arc::new(toml::from_str(toml).expect("should parse config"))
    }

    #[tokio::test]
    async fn test_router_creates() {
        let router = RuleBasedRouter::new();
        let config = test_config();
        let selector = ModelSelector::new(config);

        // Request with no rule match should use default tier
        let result = router
            .route("test", &RouteMetadata::new(100), &selector)
            .await;
        assert!(result.is_ok());
    }

    // Rule 1: Trivial/casual tasks → Fast tier
    #[test]
    fn test_casual_chat_small_tokens_routes_to_fast() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(100)
            .with_task_type(TaskType::CasualChat)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(target, Some(TargetModel::Fast));
    }

    #[test]
    fn test_casual_chat_high_importance_no_match() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(100)
            .with_task_type(TaskType::CasualChat)
            .with_importance(Importance::High);

        let target = router.evaluate_rules(&meta);
        assert_eq!(target, None); // Delegates to default tier
    }

    #[test]
    fn test_casual_chat_large_tokens_no_match() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(300)
            .with_task_type(TaskType::CasualChat)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(target, None);
    }

    // Rule 2: Medium-depth tasks → Balanced tier
    #[test]
    fn test_document_summary_routes_to_balanced() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(1500)
            .with_task_type(TaskType::DocumentSummary)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(target, Some(TargetModel::Balanced));
    }

    #[test]
    fn test_question_answer_routes_to_balanced() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(500)
            .with_task_type(TaskType::QuestionAnswer)
            .with_importance(Importance::Low);

        let target = router.evaluate_rules(&meta);
        assert_eq!(target, Some(TargetModel::Balanced));
    }

    #[test]
    fn test_medium_task_exceeds_token_limit_no_match() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(3000)
            .with_task_type(TaskType::QuestionAnswer)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(target, None);
    }

    // Rule 3: High importance or deep work → Deep tier
    #[test]
    fn test_high_importance_routes_to_deep() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(500)
            .with_task_type(TaskType::QuestionAnswer)
            .with_importance(Importance::High);

        let target = router.evaluate_rules(&meta);
        assert_eq!(target, Some(TargetModel::Deep));
    }

    #[test]
    fn test_deep_analysis_routes_to_deep() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(1000)
            .with_task_type(TaskType::DeepAnalysis)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(target, Some(TargetModel::Deep));
    }

    #[test]
    fn test_creative_writing_routes_to_deep() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(800)
            .with_task_type(TaskType::CreativeWriting)
            .with_importance(Importance::Low);

        let target = router.evaluate_rules(&meta);
        assert_eq!(target, Some(TargetModel::Deep));
    }

    // Rule 4: Code generation (special case)
    #[test]
    fn test_code_small_tokens_routes_to_balanced() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(500)
            .with_task_type(TaskType::Code)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(target, Some(TargetModel::Balanced));
    }

    #[test]
    fn test_code_large_tokens_routes_to_deep() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(2000)
            .with_task_type(TaskType::Code)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(target, Some(TargetModel::Deep));
    }

    // Property: Router should always return a valid model or None
    #[test]
    fn test_router_always_returns_valid_result() {
        let router = RuleBasedRouter::new();
        let test_cases = vec![
            (0, Importance::Low, TaskType::CasualChat),
            (100, Importance::Normal, TaskType::Code),
            (1000, Importance::High, TaskType::DeepAnalysis),
            (500, Importance::Low, TaskType::QuestionAnswer),
            (2500, Importance::Normal, TaskType::DocumentSummary),
        ];

        for (tokens, importance, task_type) in test_cases {
            let meta = RouteMetadata::new(tokens)
                .with_importance(importance)
                .with_task_type(task_type);

            let result = router.evaluate_rules(&meta);
            // Should be either Some(valid model) or None
            if let Some(model) = result {
                assert!(matches!(
                    model,
                    TargetModel::Fast | TargetModel::Balanced | TargetModel::Deep
                ));
            }
        }
    }

    // Boundary condition tests for token thresholds
    #[test]
    fn test_boundary_255_tokens_casual_chat_routes_to_fast() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(255)
            .with_task_type(TaskType::CasualChat)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(
            target,
            Some(TargetModel::Fast),
            "255 tokens should match Rule 1 (< 256)"
        );
    }

    #[test]
    fn test_boundary_256_tokens_casual_chat_no_match() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(256)
            .with_task_type(TaskType::CasualChat)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(
            target, None,
            "256 tokens should NOT match Rule 1 (requires < 256)"
        );
    }

    #[test]
    fn test_boundary_1024_tokens_code_routes_to_balanced() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(1024)
            .with_task_type(TaskType::Code)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(
            target,
            Some(TargetModel::Balanced),
            "1024 tokens should match Code → Balanced (not > 1024)"
        );
    }

    #[test]
    fn test_boundary_1025_tokens_code_routes_to_deep() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(1025)
            .with_task_type(TaskType::Code)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(
            target,
            Some(TargetModel::Deep),
            "1025 tokens should match Code → Deep (> 1024)"
        );
    }

    #[test]
    fn test_boundary_199_tokens_question_answer_no_match() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(199)
            .with_task_type(TaskType::QuestionAnswer)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(
            target, None,
            "199 tokens should NOT match Rule 4 (requires >= 200)"
        );
    }

    #[test]
    fn test_boundary_200_tokens_question_answer_routes_to_balanced() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(200)
            .with_task_type(TaskType::QuestionAnswer)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(
            target,
            Some(TargetModel::Balanced),
            "200 tokens should match Rule 4 (>= 200)"
        );
    }

    #[test]
    fn test_boundary_2047_tokens_question_answer_routes_to_balanced() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(2047)
            .with_task_type(TaskType::QuestionAnswer)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(
            target,
            Some(TargetModel::Balanced),
            "2047 tokens should match Rule 4 (< 2048)"
        );
    }

    #[test]
    fn test_boundary_2048_tokens_question_answer_no_match() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(2048)
            .with_task_type(TaskType::QuestionAnswer)
            .with_importance(Importance::Normal);

        let target = router.evaluate_rules(&meta);
        assert_eq!(
            target, None,
            "2048 tokens should NOT match Rule 4 (requires < 2048)"
        );
    }
}
