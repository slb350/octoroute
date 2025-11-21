//! Integration tests for concurrent health status updates
//!
//! Tests that concurrent mark_failure/mark_success calls maintain correct
//! consecutive_failures count and health state under concurrent load

use octoroute::{config::Config, handlers::AppState};
use std::sync::Arc;

fn create_test_config() -> Config {
    // ModelEndpoint fields are private - use TOML deserialization
    let toml = r#"
[server]
host = "127.0.0.1"
port = 8080
request_timeout_seconds = 30

[[models.fast]]
name = "concurrent-test"
base_url = "http://192.0.2.1:11434/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://192.0.2.2:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://192.0.2.3:11434/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
default_importance = "normal"
router_tier = "balanced"

[observability]
log_level = "error"
metrics_enabled = false
metrics_port = 9090
"#;
    toml::from_str(toml).expect("should parse TOML config")
}

#[tokio::test]
async fn test_concurrent_failures_maintain_correct_count() {
    // Create app state with health checker
    let config = Arc::new(create_test_config());
    let state = Arc::new(AppState::new(config.clone()).expect("AppState::new should succeed"));

    let endpoint_name = "concurrent-test";

    // Spawn 10 concurrent tasks that all call mark_failure on the same endpoint
    let mut handles = vec![];
    for _ in 0..10 {
        let state_clone: Arc<AppState> = Arc::clone(&state);
        let name = endpoint_name.to_string();
        let handle = tokio::spawn(async move {
            state_clone
                .selector()
                .health_checker()
                .mark_failure(&name)
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify consecutive_failures = 10 (not lost due to race conditions)
    let all_statuses = state.selector().health_checker().get_all_statuses().await;
    let endpoint_health = all_statuses
        .iter()
        .find(|h| h.name() == endpoint_name)
        .expect("Endpoint should exist");

    assert_eq!(
        endpoint_health.consecutive_failures(),
        10,
        "All 10 concurrent failures should be counted"
    );
    assert!(
        !endpoint_health.is_healthy(),
        "Endpoint should be unhealthy after 10 failures (>= 3 threshold)"
    );
}

#[tokio::test]
async fn test_concurrent_success_during_failures_resets_count() {
    // Create app state with health checker
    let config = Arc::new(create_test_config());
    let state = Arc::new(AppState::new(config.clone()).expect("AppState::new should succeed"));

    let endpoint_name = "concurrent-test";

    // First, mark endpoint as having 2 failures
    for _ in 0..2 {
        state
            .selector()
            .health_checker()
            .mark_failure(endpoint_name)
            .await
            .unwrap();
    }

    // Now spawn 5 concurrent failures and 1 success
    let mut handles = vec![];

    // Spawn 5 failure tasks
    for _ in 0..5 {
        let state_clone: Arc<AppState> = Arc::clone(&state);
        let name = endpoint_name.to_string();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
            state_clone
                .selector()
                .health_checker()
                .mark_failure(&name)
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    // Spawn 1 success task (runs first due to no delay)
    let state_clone: Arc<AppState> = Arc::clone(&state);
    let name = endpoint_name.to_string();
    let success_handle = tokio::spawn(async move {
        state_clone
            .selector()
            .health_checker()
            .mark_success(&name)
            .await
            .unwrap();
    });

    // Wait for success first
    success_handle.await.unwrap();

    // Wait for all failure tasks
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify final state: should have 5 consecutive failures
    // (2 initial failures were reset by success, then 5 new failures)
    let all_statuses = state.selector().health_checker().get_all_statuses().await;
    let endpoint_health = all_statuses
        .iter()
        .find(|h| h.name() == endpoint_name)
        .expect("Endpoint should exist");

    assert_eq!(
        endpoint_health.consecutive_failures(),
        5,
        "Should have 5 failures after success reset the previous 2"
    );
    assert!(
        !endpoint_health.is_healthy(),
        "Endpoint should be unhealthy with 5 consecutive failures"
    );
}

#[tokio::test]
async fn test_interleaved_success_and_failure_updates() {
    // Create app state with health checker
    let config = Arc::new(create_test_config());
    let state = Arc::new(AppState::new(config.clone()).expect("AppState::new should succeed"));

    let endpoint_name = "concurrent-test";

    // Spawn many concurrent tasks with mixed success/failure
    let mut handles = vec![];

    // 20 failures
    for i in 0..20 {
        let state_clone: Arc<AppState> = Arc::clone(&state);
        let name = endpoint_name.to_string();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_micros(i * 10)).await;
            state_clone
                .selector()
                .health_checker()
                .mark_failure(&name)
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    // 5 successes interleaved
    for i in 0..5 {
        let state_clone: Arc<AppState> = Arc::clone(&state);
        let name = endpoint_name.to_string();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_micros(i * 50)).await;
            state_clone
                .selector()
                .health_checker()
                .mark_success(&name)
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    // Wait for all tasks
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify that state is consistent (no panics, no lost updates)
    // We can't predict exact final count due to race conditions,
    // but we can verify the system handles concurrent updates gracefully
    let all_statuses = state.selector().health_checker().get_all_statuses().await;
    let endpoint_health = all_statuses
        .iter()
        .find(|h| h.name() == endpoint_name)
        .expect("Endpoint should exist");

    // Final state should be internally consistent:
    // If healthy=true, consecutive_failures should be 0
    // If healthy=false, consecutive_failures should be >= 3
    if endpoint_health.is_healthy() {
        assert_eq!(
            endpoint_health.consecutive_failures(),
            0,
            "Healthy endpoint should have 0 consecutive failures"
        );
    } else {
        assert!(
            endpoint_health.consecutive_failures() >= 3,
            "Unhealthy endpoint should have >= 3 consecutive failures"
        );
    }
}
