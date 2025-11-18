//! Rule-based routing strategy
//!
//! Fast, deterministic routing using pattern matching on metadata.
//! Zero LLM overhead - all decisions are pure CPU logic.
//!
//! Routes to generic model tiers (Fast, Balanced, Deep) based on:
//! - Task type and complexity
//! - Token count estimates
//! - User-specified importance level

use super::{Importance, RouteMetadata, TargetModel, TaskType};

/// Rule-based router that uses fast pattern matching
#[derive(Debug, Clone, Default)]
pub struct RuleBasedRouter;

impl RuleBasedRouter {
    /// Create a new rule-based router
    pub fn new() -> Self {
        Self
    }

    /// Route a request based on metadata
    ///
    /// Returns `Some(TargetModel)` if a rule matches, `None` if the request
    /// should be delegated to a more intelligent router.
    pub fn route(&self, meta: &RouteMetadata) -> Option<TargetModel> {
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

    #[test]
    fn test_router_creates() {
        let router = RuleBasedRouter::new();
        assert!(router.route(&RouteMetadata::new(100)).is_none());
    }

    // Rule 1: Trivial/casual tasks → Fast tier
    #[test]
    fn test_casual_chat_small_tokens_routes_to_fast() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(100)
            .with_task_type(TaskType::CasualChat)
            .with_importance(Importance::Normal);

        let target = router.route(&meta);
        assert_eq!(target, Some(TargetModel::Fast));
    }

    #[test]
    fn test_casual_chat_high_importance_no_match() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(100)
            .with_task_type(TaskType::CasualChat)
            .with_importance(Importance::High);

        let target = router.route(&meta);
        assert_eq!(target, None); // Delegates to LLM router
    }

    #[test]
    fn test_casual_chat_large_tokens_no_match() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(300)
            .with_task_type(TaskType::CasualChat)
            .with_importance(Importance::Normal);

        let target = router.route(&meta);
        assert_eq!(target, None);
    }

    // Rule 2: Medium-depth tasks → Balanced tier
    #[test]
    fn test_document_summary_routes_to_balanced() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(1500)
            .with_task_type(TaskType::DocumentSummary)
            .with_importance(Importance::Normal);

        let target = router.route(&meta);
        assert_eq!(target, Some(TargetModel::Balanced));
    }

    #[test]
    fn test_question_answer_routes_to_balanced() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(500)
            .with_task_type(TaskType::QuestionAnswer)
            .with_importance(Importance::Low);

        let target = router.route(&meta);
        assert_eq!(target, Some(TargetModel::Balanced));
    }

    #[test]
    fn test_medium_task_exceeds_token_limit_no_match() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(3000)
            .with_task_type(TaskType::QuestionAnswer)
            .with_importance(Importance::Normal);

        let target = router.route(&meta);
        assert_eq!(target, None);
    }

    // Rule 3: High importance or deep work → Deep tier
    #[test]
    fn test_high_importance_routes_to_deep() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(500)
            .with_task_type(TaskType::QuestionAnswer)
            .with_importance(Importance::High);

        let target = router.route(&meta);
        assert_eq!(target, Some(TargetModel::Deep));
    }

    #[test]
    fn test_deep_analysis_routes_to_deep() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(1000)
            .with_task_type(TaskType::DeepAnalysis)
            .with_importance(Importance::Normal);

        let target = router.route(&meta);
        assert_eq!(target, Some(TargetModel::Deep));
    }

    #[test]
    fn test_creative_writing_routes_to_deep() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(800)
            .with_task_type(TaskType::CreativeWriting)
            .with_importance(Importance::Low);

        let target = router.route(&meta);
        assert_eq!(target, Some(TargetModel::Deep));
    }

    // Rule 4: Code generation (special case)
    #[test]
    fn test_code_small_tokens_routes_to_balanced() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(500)
            .with_task_type(TaskType::Code)
            .with_importance(Importance::Normal);

        let target = router.route(&meta);
        assert_eq!(target, Some(TargetModel::Balanced));
    }

    #[test]
    fn test_code_large_tokens_routes_to_deep() {
        let router = RuleBasedRouter::new();
        let meta = RouteMetadata::new(2000)
            .with_task_type(TaskType::Code)
            .with_importance(Importance::Normal);

        let target = router.route(&meta);
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

            let result = router.route(&meta);
            // Should be either Some(valid model) or None
            if let Some(model) = result {
                assert!(matches!(
                    model,
                    TargetModel::Fast | TargetModel::Balanced | TargetModel::Deep
                ));
            }
        }
    }
}
