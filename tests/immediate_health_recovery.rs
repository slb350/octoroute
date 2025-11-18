//! Integration tests for immediate health recovery
//!
//! Tests that successful requests immediately mark endpoints as healthy,
//! enabling low-latency recovery from transient failures

use octoroute::{
    config::{
        Config, ModelEndpoint, ModelsConfig, ObservabilityConfig, RoutingConfig, RoutingStrategy,
        ServerConfig,
    },
    handlers::AppState,
};
use std::sync::Arc;

fn create_test_config() -> Config {
    Config {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            request_timeout_seconds: 30,
        },
        models: ModelsConfig {
            fast: vec![ModelEndpoint {
                name: "fast-recovery-test".to_string(),
                base_url: "http://192.0.2.1:11434/v1".to_string(),
                max_tokens: 2048,
                temperature: 0.7,
                weight: 1.0,
                priority: 1,
            }],
            balanced: vec![ModelEndpoint {
                name: "balanced-1".to_string(),
                base_url: "http://192.0.2.2:11434/v1".to_string(),
                max_tokens: 4096,
                temperature: 0.7,
                weight: 1.0,
                priority: 1,
            }],
            deep: vec![ModelEndpoint {
                name: "deep-1".to_string(),
                base_url: "http://192.0.2.3:11434/v1".to_string(),
                max_tokens: 8192,
                temperature: 0.7,
                weight: 1.0,
                priority: 1,
            }],
        },
        routing: RoutingConfig {
            strategy: RoutingStrategy::Rule,
            default_importance: octoroute::router::Importance::Normal,
            router_model: "balanced".to_string(),
        },
        observability: ObservabilityConfig {
            log_level: "debug".to_string(),
            metrics_enabled: false,
            metrics_port: 9090,
        },
    }
}

#[tokio::test]
async fn test_mark_success_immediately_makes_endpoint_healthy() {
    // This test verifies that mark_success() immediately marks an endpoint as healthy,
    // enabling rapid recovery without waiting for the 30-second background check

    let config = Arc::new(create_test_config());
    let state = AppState::new((*config).clone());
    let health_checker = state.selector().health_checker();

    // Mark endpoint as unhealthy (3 consecutive failures)
    health_checker
        .mark_failure("fast-recovery-test")
        .await
        .unwrap();
    health_checker
        .mark_failure("fast-recovery-test")
        .await
        .unwrap();
    health_checker
        .mark_failure("fast-recovery-test")
        .await
        .unwrap();

    // Verify it's unhealthy
    assert!(!health_checker.is_healthy("fast-recovery-test").await);

    // Mark as success - should immediately become healthy
    health_checker
        .mark_success("fast-recovery-test")
        .await
        .unwrap();

    // Verify it's healthy immediately, without waiting for background check
    assert!(
        health_checker.is_healthy("fast-recovery-test").await,
        "Endpoint should be healthy immediately after mark_success"
    );

    // Verify consecutive failures reset to 0
    let statuses = health_checker.get_all_statuses().await;
    let endpoint = statuses
        .iter()
        .find(|s| s.name() == "fast-recovery-test")
        .unwrap();
    assert_eq!(
        endpoint.consecutive_failures(),
        0,
        "Consecutive failures should be reset to 0"
    );
}

#[tokio::test]
async fn test_partial_failures_reset_on_success() {
    // This test verifies that mark_success() resets partial failure counts,
    // preventing failures from accumulating across long time periods

    let config = Arc::new(create_test_config());
    let state = AppState::new((*config).clone());
    let health_checker = state.selector().health_checker();

    // Mark 2 failures (not enough to become unhealthy)
    health_checker.mark_failure("balanced-1").await.unwrap();
    health_checker.mark_failure("balanced-1").await.unwrap();

    // Still healthy (needs 3 for unhealthy)
    assert!(health_checker.is_healthy("balanced-1").await);

    // Check failure count
    let statuses = health_checker.get_all_statuses().await;
    let endpoint = statuses.iter().find(|s| s.name() == "balanced-1").unwrap();
    assert_eq!(endpoint.consecutive_failures(), 2);

    // Mark success - should reset counter
    health_checker.mark_success("balanced-1").await.unwrap();

    // Check counter is reset
    let statuses = health_checker.get_all_statuses().await;
    let endpoint = statuses.iter().find(|s| s.name() == "balanced-1").unwrap();
    assert_eq!(
        endpoint.consecutive_failures(),
        0,
        "Success should reset consecutive failure counter"
    );
}

#[tokio::test]
async fn test_immediate_recovery_enables_low_latency_failover() {
    // This test verifies the design decision from CLAUDE.md:
    // Without immediate recovery, endpoints remain unhealthy for 0-30 seconds
    // With immediate recovery, endpoints become selectable immediately

    let config = Arc::new(create_test_config());
    let state = AppState::new((*config).clone());
    let health_checker = state.selector().health_checker();

    // Simulate a transient failure scenario:
    // 1. Endpoint has 3 consecutive failures (becomes unhealthy)
    health_checker.mark_failure("deep-1").await.unwrap();
    health_checker.mark_failure("deep-1").await.unwrap();
    health_checker.mark_failure("deep-1").await.unwrap();
    assert!(!health_checker.is_healthy("deep-1").await);

    // 2. Next request succeeds (transient failure resolved)
    health_checker.mark_success("deep-1").await.unwrap();

    // 3. Endpoint should be immediately available for selection
    assert!(
        health_checker.is_healthy("deep-1").await,
        "Immediate recovery should make endpoint selectable without delay"
    );

    // Without this feature, we'd have to wait 0-30 seconds for background check
    // With this feature, recovery is instant
}
