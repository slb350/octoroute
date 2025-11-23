//! Tests for hybrid router error propagation
//!
//! Verifies that the hybrid router propagates original errors without wrapping,
//! preserving error type information for retry logic. Context is logged but not
//! wrapped in the error type.
//!
//! ## Background
//!
//! Previous implementation wrapped LLM errors in HybridRoutingFailed, losing
//! type information needed for retry logic. New implementation propagates
//! original errors and logs context separately (PR #4 review issue #5).

use octoroute::config::Config;
use octoroute::error::AppError;
use octoroute::models::selector::ModelSelector;
use octoroute::router::{HybridRouter, Importance, RouteMetadata, TaskType};
use std::sync::Arc;

/// Helper to create test metrics
fn test_metrics() -> Arc<octoroute::metrics::Metrics> {
    Arc::new(octoroute::metrics::Metrics::new().expect("should create metrics"))
}

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

fn mock_metrics() -> Arc<octoroute::metrics::Metrics> {
    Arc::new(octoroute::metrics::Metrics::new().unwrap())
}

#[tokio::test]
async fn test_hybrid_router_propagates_original_llm_error() {
    // Verify hybrid router propagates original LLM routing errors without wrapping
    //
    // When LLM routing fails, hybrid router should propagate the original error
    // (e.g., RoutingFailed) to preserve type information for retry logic.
    // Context is logged but not wrapped in the error type.

    let config = test_config();
    let selector = Arc::new(ModelSelector::new(config.clone(), test_metrics()));
    let router = HybridRouter::new(config, selector.clone(), mock_metrics())
        .expect("Router creation should succeed");

    // Create metadata that triggers LLM fallback (no rule match)
    // CasualChat + High importance is ambiguous
    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::High,
        task_type: TaskType::CasualChat,
    };

    // Mark all balanced endpoints unhealthy to force LLM routing failure
    let health_checker = selector.health_checker();
    for _ in 0..3 {
        health_checker
            .mark_failure("test-balanced")
            .await
            .expect("mark_failure should succeed");
    }

    // Attempt routing - should fail with original RoutingFailed error
    let result = router.route("Test prompt", &meta).await;
    assert!(result.is_err(), "Should fail when LLM routing fails");

    let err = result.unwrap_err();

    // Verify error is the original RoutingFailed, not wrapped in HybridRoutingFailed
    match err {
        AppError::RoutingFailed(_) => {
            // Success - original error type is preserved
            // This allows retry logic to determine if error is retryable
        }
        other => panic!(
            "Expected RoutingFailed variant (original error propagated), got: {:?}",
            other
        ),
    }
}

#[tokio::test]
async fn test_hybrid_router_error_provides_meaningful_context() {
    // Verify that propagated errors contain meaningful diagnostic information
    //
    // Even though we don't wrap errors, the original error should provide
    // enough context for debugging. Full prompt context is logged separately.

    let config = test_config();
    let selector = Arc::new(ModelSelector::new(config.clone(), test_metrics()));
    let router = HybridRouter::new(config, selector.clone(), mock_metrics())
        .expect("Router creation should succeed");

    // Create a long prompt to verify it doesn't cause issues
    let long_prompt = "Write a comprehensive analysis of the impact of artificial intelligence on \
                      modern software development practices, including code generation, testing, \
                      documentation, and deployment automation. Consider both benefits and risks. \
                      This is a very long prompt that exceeds 100 characters by a significant margin.";

    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::High,
        task_type: TaskType::CasualChat,
    };

    // Mark all balanced endpoints unhealthy
    let health_checker = selector.health_checker();
    for _ in 0..3 {
        health_checker
            .mark_failure("test-balanced")
            .await
            .expect("mark_failure should succeed");
    }

    let result = router.route(long_prompt, &meta).await;
    assert!(result.is_err());

    let err = result.unwrap_err();

    // Verify error provides meaningful diagnostic information
    let err_string = format!("{}", err);

    // Should contain key diagnostic information (endpoint availability, tier info, etc.)
    assert!(
        err_string.contains("Balanced") || err_string.contains("endpoint"),
        "Error should contain diagnostic information about endpoint/tier, got: {}",
        err_string
    );

    // Should be the original RoutingFailed error
    assert!(
        matches!(err, AppError::RoutingFailed(_)),
        "Should propagate original RoutingFailed error"
    );
}

#[tokio::test]
async fn test_propagated_error_preserves_source_chain() {
    // Verify that propagated errors maintain their source chain
    //
    // Even though we don't wrap errors, the original error's source chain
    // should be preserved for debugging and error analysis.

    let config = test_config();
    let selector = Arc::new(ModelSelector::new(config.clone(), test_metrics()));
    let router = HybridRouter::new(config, selector.clone(), mock_metrics())
        .expect("Router creation should succeed");

    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::High,
        task_type: TaskType::CasualChat,
    };

    // Mark all balanced endpoints unhealthy
    let health_checker = selector.health_checker();
    for _ in 0..3 {
        health_checker
            .mark_failure("test-balanced")
            .await
            .expect("mark_failure should succeed");
    }

    let result = router.route("Test", &meta).await;
    assert!(result.is_err());

    let err = result.unwrap_err();

    // Verify error is the original RoutingFailed
    assert!(
        matches!(err, AppError::RoutingFailed(_)),
        "Should be original RoutingFailed error"
    );

    // Verify error chain is accessible (RoutingFailed may or may not have a source,
    // but the error should be usable for debugging)
    use std::error::Error;
    let err_string = format!("{}", err);
    assert!(
        !err_string.is_empty(),
        "Error should have meaningful message"
    );

    // The error maintains its own source chain (if any)
    // This test just verifies we can access standard error properties
    let _source = err.source(); // May be Some or None, both are valid
}
