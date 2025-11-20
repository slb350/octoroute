//! Integration tests for metrics instrumentation in chat handler
//!
//! These tests verify that metrics are correctly recorded when chat requests are processed.

#[cfg(feature = "metrics")]
mod metrics_tests {
    use octoroute::{config::Config, handlers::AppState};
    use std::sync::Arc;

    fn create_test_config() -> Config {
        let toml = r#"
[server]
host = "127.0.0.1"
port = 3000

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
router_model = "balanced"
"#;
        toml::from_str(toml).expect("should parse config")
    }

    #[tokio::test]
    async fn test_metrics_record_request_on_chat() {
        let config = Arc::new(create_test_config());
        let state = AppState::new(config).expect("should create state");

        // Get metrics before request
        let metrics = state.metrics().expect("metrics should be available");
        let output_before = metrics.gather().expect("should gather metrics");

        // Record a fake request (simulating what chat handler does)
        metrics.record_request("fast", "rule");
        metrics.record_routing_duration("rule", 1.5);
        metrics.record_model_invocation("fast");

        // Get metrics after request
        let output_after = metrics.gather().expect("should gather metrics");

        // Verify metrics changed
        assert_ne!(
            output_before, output_after,
            "Metrics should change after recording"
        );

        // Verify specific metrics exist in output
        assert!(output_after.contains("octoroute_requests_total"));
        assert!(output_after.contains("tier=\"fast\""));
        assert!(output_after.contains("strategy=\"rule\""));

        assert!(output_after.contains("octoroute_routing_duration_ms"));
        assert!(output_after.contains("strategy=\"rule\""));

        assert!(output_after.contains("octoroute_model_invocations_total"));
        assert!(output_after.contains("tier=\"fast\""));
    }

    #[tokio::test]
    async fn test_metrics_record_multiple_requests() {
        let config = Arc::new(create_test_config());
        let state = AppState::new(config).expect("should create state");

        let metrics = state.metrics().expect("metrics should be available");

        // Simulate multiple requests with different tiers and strategies
        metrics.record_request("fast", "rule");
        metrics.record_request("balanced", "llm");
        metrics.record_request("deep", "hybrid");

        metrics.record_routing_duration("rule", 0.5);
        metrics.record_routing_duration("llm", 250.0);
        metrics.record_routing_duration("hybrid", 1.0);

        metrics.record_model_invocation("fast");
        metrics.record_model_invocation("balanced");
        metrics.record_model_invocation("deep");

        let output = metrics.gather().expect("should gather metrics");

        // All tiers should be present
        assert!(output.contains("tier=\"fast\""));
        assert!(output.contains("tier=\"balanced\""));
        assert!(output.contains("tier=\"deep\""));

        // All strategies should be present
        assert!(output.contains("strategy=\"rule\""));
        assert!(output.contains("strategy=\"llm\""));
        assert!(output.contains("strategy=\"hybrid\""));
    }

    #[tokio::test]
    async fn test_metrics_routing_duration_buckets() {
        let config = Arc::new(create_test_config());
        let state = AppState::new(config).expect("should create state");

        let metrics = state.metrics().expect("metrics should be available");

        // Record durations across different buckets
        metrics.record_routing_duration("rule", 0.1); // Very fast
        metrics.record_routing_duration("rule", 1.0); // Fast
        metrics.record_routing_duration("llm", 100.0); // Slow
        metrics.record_routing_duration("llm", 500.0); // Very slow

        let output = metrics.gather().expect("should gather metrics");

        // Verify histogram buckets exist
        assert!(output.contains("le=\"0.1\""));
        assert!(output.contains("le=\"1\""));
        assert!(output.contains("le=\"100\""));
        assert!(output.contains("le=\"500\""));
        assert!(output.contains("le=\"+Inf\""));

        // Verify both strategies recorded
        assert!(output.contains("strategy=\"rule\""));
        assert!(output.contains("strategy=\"llm\""));
    }
}

// Tests when metrics feature is NOT enabled
#[cfg(not(feature = "metrics"))]
mod no_metrics_tests {
    use octoroute::{config::Config, handlers::AppState};
    use std::sync::Arc;

    fn create_test_config() -> Config {
        let toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
router_model = "balanced"
"#;
        toml::from_str(toml).expect("should parse config")
    }

    #[tokio::test]
    async fn test_metrics_not_available_without_feature() {
        let config = Arc::new(create_test_config());
        let state = AppState::new(config).expect("should create state");

        // Metrics should not be available
        assert!(
            state.metrics().is_none(),
            "Metrics should be None without feature flag"
        );
    }
}
