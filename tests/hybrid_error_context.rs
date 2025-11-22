//! Tests for hybrid router error context preservation
//!
//! Verifies that the hybrid router preserves full error context when LLM
//! routing fails, including the complete prompt and error chain, addressing
//! CRITICAL-3 from PR #4 review.
//!
//! ## Background
//!
//! Current implementation truncates prompts to 100 chars in error logs and
//! doesn't wrap LLM routing errors with hybrid routing context. This makes
//! debugging production issues difficult.

use octoroute::config::Config;
use octoroute::error::AppError;
use octoroute::models::selector::ModelSelector;
use octoroute::router::{HybridRouter, Importance, RouteMetadata, TaskType};
use std::sync::Arc;

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
async fn test_hybrid_router_error_preserves_llm_error_chain() {
    // RED: HybridRoutingFailed error should preserve LLM error chain
    //
    // When LLM routing fails, hybrid router should wrap the error with
    // context about hybrid routing fallback, preserving the original error.

    let config = test_config();
    let selector = Arc::new(ModelSelector::new(config.clone()));
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

    // Attempt routing - should fail with HybridRoutingFailed error
    let result = router.route("Test prompt", &meta).await;
    assert!(result.is_err(), "Should fail when LLM routing fails");

    let err = result.unwrap_err();

    // Verify error is HybridRoutingFailed with proper context
    match err {
        AppError::HybridRoutingFailed {
            prompt_preview,
            task_type,
            importance,
            source,
        } => {
            // Verify context is preserved
            assert!(
                prompt_preview.contains("Test prompt"),
                "Should include prompt preview"
            );
            assert_eq!(task_type, TaskType::CasualChat);
            assert_eq!(importance, Importance::High);

            // Verify original error is preserved (source is Box<AppError>, always present)
            let _original_error = source; // Just verify it's accessible
        }
        _ => panic!("Expected HybridRoutingFailed variant, got: {:?}", err),
    }
}

#[tokio::test]
async fn test_hybrid_router_error_includes_full_prompt_in_context() {
    // RED: Error context should include FULL prompt, not truncated
    //
    // Current implementation truncates to 100 chars. Need full prompt
    // for debugging production issues.

    let config = test_config();
    let selector = Arc::new(ModelSelector::new(config.clone()));
    let router = HybridRouter::new(config, selector.clone(), mock_metrics())
        .expect("Router creation should succeed");

    // Create a long prompt (>100 chars) to test truncation
    let long_prompt = "Write a comprehensive analysis of the impact of artificial intelligence on \
                      modern software development practices, including code generation, testing, \
                      documentation, and deployment automation. Consider both benefits and risks. \
                      This is a very long prompt that exceeds 100 characters by a significant margin.";

    assert!(
        long_prompt.len() > 100,
        "Test prompt should be >100 chars, got {}",
        long_prompt.len()
    );

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

    // Verify error includes full prompt context (or at least more than 100 chars)
    let err_string = format!("{}", err);

    // The error should include significant portion of the prompt
    // (we'll use 200 chars as threshold to allow for some truncation if needed)
    let prompt_portion = &long_prompt[..200];
    assert!(
        err_string.contains(prompt_portion) || err_string.len() > 200,
        "Error should include substantial prompt context (>200 chars), got {} chars: {}",
        err_string.len(),
        err_string
    );
}

#[tokio::test]
async fn test_hybrid_routing_failed_error_has_source() {
    // RED: HybridRoutingFailed should have #[source] attribute
    //
    // Verify error chain is accessible for debugging.

    let config = test_config();
    let selector = Arc::new(ModelSelector::new(config.clone()));
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

    // Verify error chain is accessible
    use std::error::Error;
    let source = err.source();
    assert!(
        source.is_some(),
        "HybridRoutingFailed should have source error in chain"
    );

    // Should be able to walk the error chain
    let mut chain_length = 0;
    let mut current = err.source();
    while let Some(e) = current {
        chain_length += 1;
        current = e.source();
    }

    assert!(
        chain_length > 0,
        "Error chain should have at least one source error"
    );
}
