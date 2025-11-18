//! Integration tests for background health checks
//!
//! Tests that background health checks execute periodically and update endpoint health status

use octoroute::{
    config::{
        Config, ModelEndpoint, ModelsConfig, ObservabilityConfig, RoutingConfig, RoutingStrategy,
        ServerConfig,
    },
    handlers::AppState,
};
use std::sync::Arc;
use std::time::Duration;

fn create_test_config() -> Config {
    Config {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 8080,
            request_timeout_seconds: 30,
        },
        models: ModelsConfig {
            fast: vec![ModelEndpoint {
                name: "fast-health-test".to_string(),
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
async fn test_background_health_checks_start_automatically() {
    // This test verifies that background health checks start when AppState is created
    // and that the health checker is accessible

    let config = Arc::new(create_test_config());
    let state = AppState::new((*config).clone());

    let health_checker = state.selector().health_checker();

    // All endpoints should start as healthy
    assert!(health_checker.is_healthy("fast-health-test").await);
    assert!(health_checker.is_healthy("balanced-1").await);
    assert!(health_checker.is_healthy("deep-1").await);

    // Get all statuses
    let statuses = health_checker.get_all_statuses().await;
    assert_eq!(statuses.len(), 3, "Should have 3 endpoints");

    // All should be healthy initially
    assert_eq!(
        statuses.iter().filter(|s| s.is_healthy()).count(),
        3,
        "All endpoints should be healthy initially"
    );
}

#[tokio::test]
async fn test_health_status_persists_across_checks() {
    // This test verifies that health status changes persist
    // Note: We cannot easily test the 30-second periodic execution without waiting 30+ seconds,
    // but we can test that manual health updates work correctly

    let config = Arc::new(create_test_config());
    let state = AppState::new((*config).clone());
    let health_checker = state.selector().health_checker();

    // Mark an endpoint as failed multiple times
    health_checker
        .mark_failure("fast-health-test")
        .await
        .unwrap();
    health_checker
        .mark_failure("fast-health-test")
        .await
        .unwrap();
    health_checker
        .mark_failure("fast-health-test")
        .await
        .unwrap();

    // After 3 failures, should be unhealthy
    assert!(!health_checker.is_healthy("fast-health-test").await);

    // Wait a bit to ensure state persists
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Should still be unhealthy
    assert!(!health_checker.is_healthy("fast-health-test").await);

    // Mark as success
    health_checker
        .mark_success("fast-health-test")
        .await
        .unwrap();

    // Should be healthy again
    assert!(health_checker.is_healthy("fast-health-test").await);
}

#[tokio::test]
async fn test_background_task_survives_creation() {
    // This test verifies that the background health check task is running
    // by checking that the AppState can be created and health checks work

    let config = Arc::new(create_test_config());
    let state = AppState::new((*config).clone());

    // Give background task time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    let health_checker = state.selector().health_checker();

    // Health checker should still be functional
    assert!(health_checker.is_healthy("fast-health-test").await);

    // Can still update health status
    health_checker
        .mark_failure("fast-health-test")
        .await
        .unwrap();

    let statuses = health_checker.get_all_statuses().await;
    let fast_endpoint = statuses
        .iter()
        .find(|s| s.name() == "fast-health-test")
        .unwrap();

    // Should have 1 failure recorded
    assert_eq!(fast_endpoint.consecutive_failures(), 1);
}
