//! Hybrid router combining rule-based and LLM-based strategies
//!
//! Tries rule-based routing first (fast path), falls back to LLM-based
//! routing for ambiguous cases.

use crate::config::Config;
use crate::error::AppResult;
use crate::models::selector::ModelSelector;
use crate::router::{LlmBasedRouter, LlmRouter, RouteMetadata, RoutingDecision, RuleBasedRouter};
use std::sync::Arc;

/// Hybrid router combining rule-based and LLM-based strategies
///
/// Provides the best of both worlds: fast deterministic routing via rules,
/// with intelligent LLM fallback for ambiguous cases.
pub struct HybridRouter {
    rule_router: RuleBasedRouter,
    llm_router: Arc<dyn LlmRouter>,
    selector: Arc<ModelSelector>,
}

impl HybridRouter {
    /// Create a new hybrid router with default LLM-based router
    ///
    /// Returns an error if LLM router construction fails
    /// (e.g., no endpoints configured for the router tier).
    ///
    /// The router tier is determined by `config.routing.router_tier`.
    pub fn new(
        config: Arc<Config>,
        selector: Arc<ModelSelector>,
        metrics: Arc<crate::metrics::Metrics>,
    ) -> AppResult<Self> {
        // Router tier from config (serde validates format at deserialization time)
        let router_tier = config.routing.router_tier();

        let llm_router = LlmBasedRouter::new(selector.clone(), router_tier, metrics)?;
        Ok(Self {
            rule_router: RuleBasedRouter::new(),
            llm_router: Arc::new(llm_router),
            selector,
        })
    }

    /// Create a new hybrid router with a custom LLM router implementation
    ///
    /// This constructor allows dependency injection for testing purposes.
    /// In production, use `new()` which creates a standard LlmBasedRouter.
    #[cfg(test)]
    pub fn new_with_llm_router(
        llm_router: Arc<dyn LlmRouter>,
        selector: Arc<ModelSelector>,
    ) -> Self {
        Self {
            rule_router: RuleBasedRouter::new(),
            llm_router,
            selector,
        }
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
    /// - LLM fallback: Prevents defaulting to BALANCED for ambiguous cases. For example,
    ///   "Write a 10,000-word analysis of quantum computing" has no matching rule (high tokens,
    ///   ambiguous task type). Defaulting to BALANCED would waste compute and produce poor results.
    ///   LLM router intelligently routes to DEEP, justifying the 100-500ms latency overhead.
    ///
    /// Returns a RoutingDecision containing the target model tier and
    /// the strategy that was used (Rule or Llm).
    pub async fn route(
        &self,
        user_prompt: &str,
        meta: &RouteMetadata,
    ) -> AppResult<RoutingDecision> {
        // Try rule-based first (fast path)
        // Rule router returns Ok(Some) if rule matched, Ok(None) if no match
        match self
            .rule_router
            .route(user_prompt, meta, &self.selector)
            .await?
        {
            Some(decision) => {
                // Rule matched - use it immediately
                tracing::info!(
                    target = ?decision.target(),
                    strategy = ?decision.strategy(),
                    token_estimate = meta.token_estimate,
                    importance = ?meta.importance,
                    task_type = ?meta.task_type,
                    "Route decision made via rule-based routing (rule matched)"
                );
                Ok(decision)
            }
            None => {
                // No rule matched - fall back to LLM router
                // Log why rules didn't match to help operators tune rule thresholds
                tracing::info!(
                    token_estimate = meta.token_estimate,
                    importance = ?meta.importance,
                    task_type = ?meta.task_type,
                    "No rule matched (token_estimate={}, task_type={:?}, importance={:?}), \
                     delegating to LLM router for intelligent routing. \
                     Rules evaluated: CasualChat+low_tokens → Fast, Code+medium_tokens → Balanced, \
                     DeepAnalysis/CreativeWriting/HighImportance → Deep",
                    meta.token_estimate,
                    meta.task_type,
                    meta.importance
                );

                let decision = self
                    .llm_router
                    .route(user_prompt, meta)
                    .await
                    .map_err(|e| {
                        // Log hybrid routing context but propagate original error
                        // This preserves error type information for retry logic to determine
                        // if error is retryable (network timeout) vs systemic (invalid config)
                        tracing::error!(
                            error = %e,
                            error_type = std::any::type_name_of_val(&e),
                            user_prompt = user_prompt,  // Full prompt, no truncation
                            task_type = ?meta.task_type,
                            importance = ?meta.importance,
                            token_estimate = meta.token_estimate,
                            "LLM router failed after no rule match (hybrid routing fallback failed)"
                        );

                        // Propagate original error to preserve type information for retry logic
                        e
                    })?;

                tracing::info!(
                    target = ?decision.target(),
                    strategy = ?decision.strategy(),
                    token_estimate = meta.token_estimate,
                    importance = ?meta.importance,
                    task_type = ?meta.task_type,
                    "Route decision made via LLM-based routing"
                );

                Ok(decision)
            }
        }
    }
}

#[cfg(test)]
mod tests {

    fn mock_metrics() -> Arc<crate::metrics::Metrics> {
        Arc::new(crate::metrics::Metrics::new().unwrap())
    }
    use super::*;
    use crate::config::Config;
    use crate::models::selector::ModelSelector;
    use crate::router::{
        Importance, RouteMetadata, RoutingDecision, RoutingStrategy, TargetModel, TaskType,
    };
    use async_trait::async_trait;

    /// Mock LLM router for testing
    ///
    /// Returns a fixed routing decision without making any network calls.
    /// This allows testing the hybrid router fallback logic without dependencies
    /// on external services.
    struct MockLlmRouter {
        /// The target model to return for all routing requests
        fixed_target: TargetModel,
    }

    impl MockLlmRouter {
        fn new(fixed_target: TargetModel) -> Self {
            Self { fixed_target }
        }
    }

    #[async_trait]
    impl LlmRouter for MockLlmRouter {
        async fn route(
            &self,
            _user_prompt: &str,
            _meta: &RouteMetadata,
        ) -> AppResult<RoutingDecision> {
            // Always return the fixed target tier with LLM strategy
            Ok(RoutingDecision::new(
                self.fixed_target,
                RoutingStrategy::Llm,
            ))
        }
    }

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
            router_tier = "balanced"

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
        let selector = Arc::new(ModelSelector::new(config.clone(), mock_metrics()));
        let _router = HybridRouter::new(config, selector, mock_metrics())
            .expect("HybridRouter::new should succeed with balanced tier");
        // If we get here without panic, creation succeeded
    }

    #[tokio::test]
    async fn test_hybrid_router_uses_rule_when_matched() {
        let config = test_config();
        let selector = Arc::new(ModelSelector::new(config.clone(), mock_metrics()));
        let router = HybridRouter::new(config, selector, mock_metrics())
            .expect("HybridRouter::new should succeed");

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
        let selector = Arc::new(ModelSelector::new(config.clone(), mock_metrics()));
        let router = HybridRouter::new(config, selector, mock_metrics())
            .expect("HybridRouter::new should succeed");

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
        let selector = Arc::new(ModelSelector::new(config.clone(), mock_metrics()));
        let router = HybridRouter::new(config, selector, mock_metrics())
            .expect("HybridRouter::new should succeed");

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
        let selector = Arc::new(ModelSelector::new(config.clone(), mock_metrics()));
        let router = HybridRouter::new(config, selector, mock_metrics())
            .expect("HybridRouter::new should succeed");

        // Verify router has both components (indirectly via compilation)
        // If this compiles and creates, both routers were constructed successfully
        let _rule = &router.rule_router;
        let _llm = &router.llm_router;
    }

    #[tokio::test]
    async fn test_hybrid_router_both_rule_and_llm_fail() {
        let config = test_config();
        let selector = Arc::new(ModelSelector::new(config.clone(), mock_metrics()));
        let router = HybridRouter::new(config, selector.clone(), mock_metrics())
            .expect("HybridRouter::new should succeed");

        // 1. Create metadata that triggers LLM fallback (no rule match)
        // CasualChat + High importance is explicitly ambiguous (see rule_based.rs line 103)
        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::High,
            task_type: TaskType::CasualChat,
        };

        // 2. Mark ALL endpoints unhealthy (3 consecutive failures each)
        // This ensures LLM router fails (no healthy balanced endpoints)
        let health_checker = selector.health_checker();
        for endpoint_name in ["test-fast", "test-balanced", "test-deep"] {
            for _ in 0..3 {
                health_checker
                    .mark_failure(endpoint_name)
                    .await
                    .expect("mark_failure should succeed");
            }
        }

        // 3. Attempt routing - should fail because:
        //    - Rule router: returns None (no rule matches)
        //    - Hybrid router: delegates to LLM router
        //    - LLM router: fails (no healthy balanced endpoints)
        let result = router.route("test prompt", &meta).await;

        assert!(
            result.is_err(),
            "Should fail when LLM router has no healthy balanced endpoints"
        );

        // 4. Verify error message is informative
        let err = result.unwrap_err();
        let err_msg = format!("{}", err);

        // Error should indicate routing failure (not some other error type)
        assert!(
            err_msg.contains("routing")
                || err_msg.contains("router")
                || err_msg.contains("available")
                || err_msg.contains("healthy")
                || err_msg.contains("balanced") // LLM router specifically needs balanced tier
                || err_msg.contains("Balanced"),
            "Error should indicate routing failure, got: {}",
            err_msg
        );
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // TDD: Tests for Option 1 - HybridRouter LLM fallback when no rule matches
    // ═══════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_hybrid_router_llm_fallback_on_no_rule_match() {
        // This test verifies the CRITICAL FIX: when no rule matches, hybrid router
        // should call LLM router, NOT use default tier.
        //
        // Bug: Previously, rule router used default_tier() when no match, so hybrid
        //      router never called LLM. This test ensures LLM fallback works.
        //
        // FIX: Use MockLlmRouter to avoid network calls and panics during testing.

        let config = test_config();
        let selector = Arc::new(ModelSelector::new(config.clone(), mock_metrics()));

        // Create mock LLM router that returns Balanced tier
        let mock_llm_router = Arc::new(MockLlmRouter::new(TargetModel::Balanced));

        // Create hybrid router with mock LLM router (no network calls)
        let router = HybridRouter::new_with_llm_router(mock_llm_router, selector.clone());

        // CasualChat + High importance has NO rule match (see rule_based.rs line 103)
        // This is the "ambiguous case" that should trigger LLM routing
        let meta = RouteMetadata {
            token_estimate: 100,
            importance: Importance::High,
            task_type: TaskType::CasualChat,
        };

        // Attempt routing
        // Expected behavior:
        // 1. Rule router returns None (no match)
        // 2. Hybrid router sees None, calls mock LLM router
        // 3. Mock LLM router returns Balanced tier (no network call)
        let result = router.route("Am I allowed to do this?", &meta).await;

        // Result should succeed (mock router doesn't make network calls)
        assert!(
            result.is_ok(),
            "Should succeed with mock LLM router: {:?}",
            result
        );

        let decision = result.unwrap();

        // Verify LLM router was called (strategy should be Llm, not Rule)
        assert_eq!(
            decision.strategy(),
            RoutingStrategy::Llm,
            "Strategy should be Llm (proves LLM router was called)"
        );

        // Verify the mock router's decision was used
        assert_eq!(
            decision.target(),
            TargetModel::Balanced,
            "Target should be Balanced (from mock LLM router)"
        );
    }

    #[tokio::test]
    async fn test_hybrid_router_rule_match_skips_llm() {
        // Verify that when a rule DOES match, LLM is NOT called (fast path works)
        let config = test_config();
        let selector = Arc::new(ModelSelector::new(config.clone(), mock_metrics()));
        let router = HybridRouter::new(config, selector, mock_metrics())
            .expect("HybridRouter::new should succeed");

        // CasualChat + Low + <256 tokens matches Rule 1 → Fast
        let meta = RouteMetadata {
            token_estimate: 50,
            importance: Importance::Low,
            task_type: TaskType::CasualChat,
        };

        let result = router.route("Hi there", &meta).await;

        // Should succeed without querying any endpoints (rule match is synchronous)
        assert!(
            result.is_ok(),
            "Rule match should succeed without endpoint query"
        );

        let decision = result.unwrap();
        assert_eq!(
            decision.target(),
            TargetModel::Fast,
            "Should route to Fast tier"
        );
        assert_eq!(
            decision.strategy(),
            RoutingStrategy::Rule,
            "Should use Rule strategy (not Llm)"
        );
    }
}
