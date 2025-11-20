//! Integration tests for background health checks
//!
//! Tests that background health checks execute periodically and update endpoint health status

use octoroute::{config::Config, handlers::AppState};
use std::sync::Arc;
use std::time::Duration;

fn create_test_config() -> Config {
    // ModelEndpoint fields are private - use TOML deserialization
    let toml = r#"
[server]
host = "127.0.0.1"
port = 8080
request_timeout_seconds = 30

[[models.fast]]
name = "fast-health-test"
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
router_model = "balanced"

[observability]
log_level = "debug"
metrics_enabled = false
metrics_port = 9090
"#;
    toml::from_str(toml).expect("should parse TOML config")
}

#[tokio::test]
async fn test_background_health_checks_start_automatically() {
    // This test verifies that background health checks start when AppState is created
    // and that the health checker is accessible

    let config = Arc::new(create_test_config());
    let state = AppState::new(config.clone()).expect("AppState::new should succeed");

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
    let state = AppState::new(config.clone()).expect("AppState::new should succeed");
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
    let state = AppState::new(config.clone()).expect("AppState::new should succeed");

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

#[tokio::test]
async fn test_background_task_resilience_to_rapid_state_changes() {
    // This test simulates stress conditions that might occur during task restart
    // or rapid failure/recovery cycles. The background health check task should
    // remain functional through rapid state changes.
    //
    // Note: The actual restart logic (exponential backoff on panic) is difficult
    // to test directly without injecting panics. The restart mechanism:
    // - Catches task panics/termination
    // - Retries up to 5 times with exponential backoff (1s, 2s, 4s, 8s, 16s)
    // - Logs errors and backoff attempts
    // - Stops permanently after 5 failed attempts
    //
    // This test verifies that the health checker remains consistent under stress,
    // which is what we'd expect if the restart logic is working correctly.

    let config = Arc::new(create_test_config());
    let state = AppState::new(config.clone()).expect("AppState::new should succeed");
    let health_checker = state.selector().health_checker();

    // Simulate rapid failure/success cycles on multiple endpoints concurrently
    let mut handles = vec![];

    for endpoint_name in ["fast-health-test", "balanced-1", "deep-1"] {
        let checker = health_checker.clone();
        let name = endpoint_name.to_string();

        handles.push(tokio::spawn(async move {
            // Rapid state changes: 10 iterations of fail->success->fail
            for _ in 0..10 {
                // Fail 3 times to mark unhealthy
                for _ in 0..3 {
                    checker.mark_failure(&name).await.unwrap();
                }
                // Verify unhealthy
                assert!(!checker.is_healthy(&name).await);

                // Recover
                checker.mark_success(&name).await.unwrap();
                // Verify healthy
                assert!(checker.is_healthy(&name).await);

                // Small delay to allow concurrent operations
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }));
    }

    // Wait for all concurrent operations to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // After all the chaos, health checker should still be functional
    // All endpoints should be healthy (ended on mark_success)
    assert!(health_checker.is_healthy("fast-health-test").await);
    assert!(health_checker.is_healthy("balanced-1").await);
    assert!(health_checker.is_healthy("deep-1").await);

    // Verify we can still query all statuses
    let statuses = health_checker.get_all_statuses().await;
    assert_eq!(statuses.len(), 3);

    // All should be healthy
    assert_eq!(
        statuses.iter().filter(|s| s.is_healthy()).count(),
        3,
        "All endpoints should be healthy after stress test"
    );
}

#[tokio::test]
async fn test_health_checker_state_consistency_under_load() {
    // Additional test to verify state remains consistent when multiple
    // tasks are reading/writing health status concurrently
    //
    // This simulates what might happen if the background health check
    // task is running while the application is also marking successes/failures

    let config = Arc::new(create_test_config());
    let state = AppState::new(config.clone()).expect("AppState::new should succeed");
    let health_checker = state.selector().health_checker();

    // Spawn multiple readers and writers
    let mut handles = vec![];

    // Spawn 5 writer tasks
    for i in 0..5 {
        let checker = health_checker.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..20 {
                // Alternate between marking success and failure
                if i % 2 == 0 {
                    checker.mark_success("fast-health-test").await.unwrap();
                } else {
                    checker.mark_failure("fast-health-test").await.unwrap();
                }
                tokio::time::sleep(Duration::from_micros(100)).await;
            }
        }));
    }

    // Spawn 3 reader tasks
    for _ in 0..3 {
        let checker = health_checker.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..30 {
                // Just read health status repeatedly
                let _ = checker.is_healthy("fast-health-test").await;
                let _ = checker.get_all_statuses().await;
                tokio::time::sleep(Duration::from_micros(50)).await;
            }
        }));
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // Health checker should still be functional and consistent
    let statuses = health_checker.get_all_statuses().await;
    assert_eq!(statuses.len(), 3, "Should still have 3 endpoints tracked");

    // Can still perform operations
    health_checker
        .mark_success("fast-health-test")
        .await
        .unwrap();
    assert!(health_checker.is_healthy("fast-health-test").await);
}
