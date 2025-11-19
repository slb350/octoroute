//! Test for concurrent health state mutations
//!
//! Verifies that RwLock prevents data corruption when mark_success and mark_failure
//! are called concurrently on the same endpoint.

use octoroute::config::Config;
use octoroute::models::HealthChecker;
use std::sync::Arc;

fn create_test_config() -> Config {
    let toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:1234/v1"
max_tokens = 2048

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
router_model = "balanced"
"#;
    toml::from_str(toml).expect("should parse TOML")
}

#[tokio::test]
async fn test_concurrent_mark_success_and_failure_no_corruption() {
    // This test verifies that concurrent calls to mark_success and mark_failure
    // on the same endpoint don't cause data corruption or panics due to the RwLock.
    //
    // Setup: Spawn two tasks that concurrently call mark_success and mark_failure
    // on the same endpoint many times.
    //
    // Expected: No panics, final state is one of the valid states (healthy or unhealthy)

    let config = Arc::new(create_test_config());
    let checker = Arc::new(HealthChecker::new(config));

    const ITERATIONS: usize = 100;

    // Spawn task that repeatedly marks as success
    let checker_success = checker.clone();
    let success_task = tokio::spawn(async move {
        for _ in 0..ITERATIONS {
            let _ = checker_success.mark_success("fast-1").await;
        }
    });

    // Spawn task that repeatedly marks as failure
    let checker_failure = checker.clone();
    let failure_task = tokio::spawn(async move {
        for _ in 0..ITERATIONS {
            let _ = checker_failure.mark_failure("fast-1").await;
        }
    });

    // Wait for both tasks to complete
    success_task.await.unwrap();
    failure_task.await.unwrap();

    // Verify endpoint is in a valid state (either healthy or unhealthy, but not corrupted)
    // We can't predict which state it will be in due to race conditions, but
    // the important thing is that we can query it without panicking
    let _is_healthy = checker.is_healthy("fast-1").await;
    // If we got here without panicking, the test passed

    println!("✓ Concurrent health mutations completed without corruption or panics");
}

#[tokio::test]
async fn test_concurrent_health_checks_maintain_correct_final_state() {
    // More deterministic test: After concurrent updates, perform deterministic
    // operations and verify they work correctly

    let config = Arc::new(create_test_config());
    let checker = Arc::new(HealthChecker::new(config));

    // Concurrent updates
    let checker_1 = checker.clone();
    let task_1 = tokio::spawn(async move {
        for _ in 0..50 {
            let _ = checker_1.mark_failure("fast-1").await;
        }
    });

    let checker_2 = checker.clone();
    let task_2 = tokio::spawn(async move {
        for _ in 0..50 {
            let _ = checker_2.mark_success("fast-1").await;
        }
    });

    task_1.await.unwrap();
    task_2.await.unwrap();

    // Now do deterministic operations
    // Reset to healthy
    checker.mark_success("fast-1").await.unwrap();
    assert!(checker.is_healthy("fast-1").await);

    // Three failures should make it unhealthy
    checker.mark_failure("fast-1").await.unwrap();
    checker.mark_failure("fast-1").await.unwrap();
    checker.mark_failure("fast-1").await.unwrap();
    assert!(!checker.is_healthy("fast-1").await);

    // One success should recover
    checker.mark_success("fast-1").await.unwrap();
    assert!(checker.is_healthy("fast-1").await);

    println!("✓ Health checker maintains correct state after concurrent mutations");
}
