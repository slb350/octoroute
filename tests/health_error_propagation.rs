//! Tests for health tracking error propagation
//!
//! Verifies that health tracking failures in the background health checking task
//! are surfaced via metrics (not just logged).
//!
//! Addresses PR #4 Issue: Background task failures logged but not surfaced

use octoroute::config::Config;
use octoroute::metrics::Metrics;
use octoroute::models::health::HealthChecker;
use std::sync::Arc;

/// Helper to create a test config
fn create_test_config() -> Config {
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1235/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:1236/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
default_importance = "normal"
"#;

    toml::from_str(config_toml).expect("should parse test config")
}

/// Test that HealthChecker can be constructed with metrics
///
/// **RED PHASE**: This will fail because new_with_metrics() doesn't exist yet
#[tokio::test]
async fn test_health_checker_construction_with_metrics() {
    let config = Arc::new(create_test_config());
    let metrics = Arc::new(Metrics::new().expect("should create metrics"));

    // Should be able to construct HealthChecker with metrics reference
    let _health_checker = HealthChecker::new_with_metrics(config, metrics);

    // If we get here, construction succeeded
}
