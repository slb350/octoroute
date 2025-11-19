//! Hybrid router combining rule-based and LLM-based strategies
//!
//! Tries rule-based routing first (fast path), falls back to LLM-based
//! routing for ambiguous cases.

use crate::config::Config;
use crate::error::AppResult;
use crate::models::selector::ModelSelector;
use crate::router::{
    LlmBasedRouter, RouteMetadata, RoutingDecision, RoutingStrategy, RuleBasedRouter,
};
use std::sync::Arc;

/// Hybrid router combining rule-based and LLM-based strategies
///
/// Provides the best of both worlds: fast deterministic routing via rules,
/// with intelligent LLM fallback for ambiguous cases.
pub struct HybridRouter {
    rule_router: RuleBasedRouter,
    llm_router: LlmBasedRouter,
}

impl HybridRouter {
    /// Create a new hybrid router
    ///
    /// Returns an error if LLM router construction fails
    /// (e.g., no balanced tier endpoints configured).
    pub fn new(_config: Arc<Config>, selector: Arc<ModelSelector>) -> AppResult<Self> {
        Ok(Self {
            rule_router: RuleBasedRouter::new(),
            llm_router: LlmBasedRouter::new(selector)?,
        })
    }

    /// Route using hybrid strategy
    ///
    /// # Routing Logic
    /// 1. **Fast Path (Rule-Based)**: Tries rule-based routing first for deterministic,
    ///    zero-latency decisions. Rules cover ~70-80% of common cases.
    ///
    /// 2. **Intelligent Fallback (LLM-Based)**: When rules return None (ambiguous cases),
    ///    delegates to LLM router. The LLM uses semantic analysis to make an intelligent
    ///    routing decision. This adds ~100-500ms latency but prevents poor routing.
    ///
    /// # Why This Order?
    /// - Rule-based first: Zero-latency for obvious cases, saves LLM calls for ~70-80% of requests
    /// - LLM fallback: Prevents defaulting to BALANCED for ambiguous cases (which could waste
    ///   compute by routing complex analysis to medium model)
    ///
    /// Returns a RoutingDecision containing the target model tier and
    /// the strategy that was used (Rule or Llm).
    pub async fn route(
        &self,
        user_prompt: &str,
        meta: &RouteMetadata,
    ) -> AppResult<RoutingDecision> {
        // Try rule-based first (fast path)
        if let Some(target) = self.rule_router.route(meta) {
            tracing::info!(
                target = ?target,
                strategy = "rule",
                token_estimate = meta.token_estimate,
                importance = ?meta.importance,
                task_type = ?meta.task_type,
                "Route decision made via rule-based routing"
            );
            return Ok(RoutingDecision::new(target, RoutingStrategy::Rule));
        }

        // Fall back to LLM router for ambiguous cases
        tracing::debug!(
            token_estimate = meta.token_estimate,
            importance = ?meta.importance,
            task_type = ?meta.task_type,
            "No rule matched, delegating to LLM router"
        );

        let target = self
            .llm_router
            .route(user_prompt, meta)
            .await
            .map_err(|e| {
                tracing::error!(
                    error = %e,
                    user_prompt_preview = &user_prompt.chars().take(100).collect::<String>(),
                    task_type = ?meta.task_type,
                    importance = ?meta.importance,
                    token_estimate = meta.token_estimate,
                    "LLM router failed after rule router returned None"
                );
                e
            })?;

        tracing::info!(
            target = ?target,
            strategy = "llm",
            token_estimate = meta.token_estimate,
            importance = ?meta.importance,
            task_type = ?meta.task_type,
            "Route decision made via LLM-based routing"
        );

        Ok(RoutingDecision::new(target, RoutingStrategy::Llm))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::models::selector::ModelSelector;
    use crate::router::{Importance, RoutingStrategy, TargetModel, TaskType};

    /// Helper to create test config
    fn test_config() -> Arc<Config> {
        let config_str = r#"
            [server]
            host = "0.0.0.0"
            port = 3000
            request_timeout_seconds = 30

            [[models.fast]]
            name = "test-fast"
            base_url = "http://localhost:11434/v1"
            max_tokens = 4096
            temperature = 0.7
            weight = 1.0
            priority = 1

            [[models.balanced]]
            name = "test-balanced"
            base_url = "http://localhost:1234/v1"
            max_tokens = 8192
            temperature = 0.7
            weight = 1.0
            priority = 1

            [[models.deep]]
            name = "test-deep"
            base_url = "http://localhost:8080/v1"
            max_tokens = 16384
            temperature = 0.7
            weight = 1.0
            priority = 1

            [routing]
            strategy = "hybrid"
            default_importance = "normal"
            router_model = "balanced"

            [observability]
            log_level = "info"
            metrics_enabled = false
            metrics_port = 9090
        "#;

        let config: Config = toml::from_str(config_str).unwrap();
        Arc::new(config)
    }

    #[tokio::test]
    async fn test_hybrid_router_creation() {
        let config = test_config();
        let selector = Arc::new(ModelSelector::new(config.clone()));
        let _router = HybridRouter::new(config, selector)
            .expect("HybridRouter::new should succeed with balanced tier");
        // If we get here without panic, creation succeeded
    }

    #[tokio::test]
    async fn test_hybrid_router_uses_rule_when_matched() {
        let config = test_config();
        let selector = Arc::new(ModelSelector::new(config.clone()));
        let router = HybridRouter::new(config, selector).expect("HybridRouter::new should succeed");

        // Simple casual chat should match rule-based routing
        let meta = RouteMetadata {
            token_estimate: 50,
            importance: Importance::Low,
            task_type: TaskType::CasualChat,
        };

        let result = router.route("Hello!", &meta).await;
        assert!(result.is_ok());

        let decision = result.unwrap();
        assert_eq!(decision.target(), TargetModel::Fast);
        assert_eq!(decision.strategy(), RoutingStrategy::Rule);
    }

    #[tokio::test]
    async fn test_hybrid_router_uses_rule_for_code() {
        let config = test_config();
        let selector = Arc::new(ModelSelector::new(config.clone()));
        let router = HybridRouter::new(config, selector).expect("HybridRouter::new should succeed");

        // Short code task should match rule-based routing
        let meta = RouteMetadata {
            token_estimate: 512,
            importance: Importance::Normal,
            task_type: TaskType::Code,
        };

        let result = router.route("Write a hello world function", &meta).await;
        assert!(result.is_ok());

        let decision = result.unwrap();
        assert_eq!(decision.target(), TargetModel::Balanced);
        assert_eq!(decision.strategy(), RoutingStrategy::Rule);
    }

    #[tokio::test]
    async fn test_hybrid_router_uses_rule_for_high_importance() {
        let config = test_config();
        let selector = Arc::new(ModelSelector::new(config.clone()));
        let router = HybridRouter::new(config, selector).expect("HybridRouter::new should succeed");

        // High importance should match rule-based routing
        let meta = RouteMetadata {
            token_estimate: 500,
            importance: Importance::High,
            task_type: TaskType::QuestionAnswer,
        };

        let result = router.route("Important question", &meta).await;
        assert!(result.is_ok());

        let decision = result.unwrap();
        assert_eq!(decision.target(), TargetModel::Deep);
        assert_eq!(decision.strategy(), RoutingStrategy::Rule);
    }

    // Note: We cannot easily test the LLM fallback path without mocking
    // the LLM response, which would require significant test infrastructure.
    // Integration tests will cover the LLM path with real endpoints.

    #[tokio::test]
    async fn test_hybrid_router_has_both_routers() {
        let config = test_config();
        let selector = Arc::new(ModelSelector::new(config.clone()));
        let router = HybridRouter::new(config, selector).expect("HybridRouter::new should succeed");

        // Verify router has both components (indirectly via compilation)
        // If this compiles and creates, both routers were constructed successfully
        let _rule = &router.rule_router;
        let _llm = &router.llm_router;
    }
}
