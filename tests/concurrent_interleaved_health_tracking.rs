//! Simple test for concurrent health tracking operations
//!
//! Verifies no panics/deadlocks when mark_success() and mark_failure()
//! are called concurrently on the same endpoint.

use octoroute::config::Config;
use octoroute::models::health::HealthChecker;
use std::sync::Arc;

fn create_test_config() -> Config {
    let toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "test-endpoint"
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
"#;
    toml::from_str(toml).expect("should parse test config")
}

/// Test concurrent mark_success and mark_failure calls don't panic/deadlock
#[tokio::test]
async fn test_concurrent_health_tracking_no_panic() {
    let config = Arc::new(create_test_config());
    let metrics = Arc::new(octoroute::metrics::Metrics::new().expect("should create metrics"));
    let health_checker = Arc::new(HealthChecker::new_with_metrics(config, metrics));

    const ENDPOINT_NAME: &str = "test-endpoint";

    // Spawn 10 concurrent tasks (5 success, 5 failure)
    let mut handles = vec![];

    for i in 0..10 {
        let checker = health_checker.clone();
        handles.push(tokio::spawn(async move {
            if i % 2 == 0 {
                let _ = checker.mark_success(ENDPOINT_NAME).await;
            } else {
                let _ = checker.mark_failure(ENDPOINT_NAME).await;
            }
        }));
    }

    // Wait for all - key test is no panic/deadlock
    for handle in handles {
        handle.await.expect("task should not panic");
    }

    // Verify state is readable (no deadlock)
    let _ = health_checker.is_healthy(ENDPOINT_NAME).await;
}
