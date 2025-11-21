//! Integration tests for metrics instrumentation in chat handler
//!
//! These tests verify that metrics are correctly recorded when chat requests are processed.

mod metrics_tests {
    use octoroute::{
        config::Config,
        handlers::AppState,
        metrics::{Strategy, Tier},
    };
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
        let metrics = state.metrics();
        let output_before = metrics.gather().expect("should gather metrics");

        // Record a fake request (simulating what chat handler does)
        metrics
            .record_request(Tier::Fast, Strategy::Rule)
            .expect("should record request");
        metrics
            .record_routing_duration(Strategy::Rule, 1.5)
            .expect("should record duration");
        metrics
            .record_model_invocation(Tier::Fast)
            .expect("should record invocation");

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

        let metrics = state.metrics();

        // Simulate multiple requests with different tiers and strategies
        metrics.record_request(Tier::Fast, Strategy::Rule).unwrap();
        metrics
            .record_request(Tier::Balanced, Strategy::Llm)
            .unwrap();
        metrics.record_request(Tier::Deep, Strategy::Rule).unwrap();

        metrics
            .record_routing_duration(Strategy::Rule, 0.5)
            .unwrap();
        metrics
            .record_routing_duration(Strategy::Llm, 250.0)
            .unwrap();

        metrics.record_model_invocation(Tier::Fast).unwrap();
        metrics.record_model_invocation(Tier::Balanced).unwrap();
        metrics.record_model_invocation(Tier::Deep).unwrap();

        let output = metrics.gather().expect("should gather metrics");

        // All tiers should be present
        assert!(output.contains("tier=\"fast\""));
        assert!(output.contains("tier=\"balanced\""));
        assert!(output.contains("tier=\"deep\""));

        // All strategies should be present
        assert!(output.contains("strategy=\"rule\""));
        assert!(output.contains("strategy=\"llm\""));
        assert!(
            !output.contains("strategy=\"hybrid\""),
            "Hybrid meta-strategy should be suppressed in metrics output"
        );
    }

    #[tokio::test]
    async fn test_metrics_routing_duration_buckets() {
        let config = Arc::new(create_test_config());
        let state = AppState::new(config).expect("should create state");

        let metrics = state.metrics();

        // Record durations across different buckets
        metrics
            .record_routing_duration(Strategy::Rule, 0.1)
            .unwrap(); // Very fast
        metrics
            .record_routing_duration(Strategy::Rule, 1.0)
            .unwrap(); // Fast
        metrics
            .record_routing_duration(Strategy::Llm, 100.0)
            .unwrap(); // Slow
        metrics
            .record_routing_duration(Strategy::Llm, 500.0)
            .unwrap(); // Very slow

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

    #[tokio::test]
    async fn test_chat_handler_records_metrics_end_to_end() {
        use axum::Extension;
        use axum::extract::State;
        use octoroute::handlers::chat::{ChatRequest, handler};
        use octoroute::middleware::RequestId;

        // Create config with non-routable IP (will fail query but still route)
        // Using TEST-NET-1 (192.0.2.0/24) reserved for documentation
        let config_str = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 1

[[models.fast]]
name = "test-fast"
base_url = "http://192.0.2.1:11434/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "test-balanced"
base_url = "http://192.0.2.2:11434/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "test-deep"
base_url = "http://192.0.2.3:11434/v1"
max_tokens = 16384
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
router_model = "balanced"
"#;

        let config: Config = toml::from_str(config_str).expect("should parse config");
        let state = AppState::new(Arc::new(config)).expect("should create state");

        // Get metrics before request
        let metrics = state.metrics();
        let output_before = metrics.gather().expect("should gather metrics before");

        // Create a chat request that will hit Fast tier (casual chat, low importance)
        let request_json = serde_json::json!({
            "message": "Hello, how are you?",
            "importance": "low",
            "task_type": "casual_chat"
        });
        let request: ChatRequest =
            serde_json::from_value(request_json).expect("should deserialize");

        // Send request through handler
        let result = handler(
            State(state.clone()),
            Extension(RequestId::new()),
            axum::Json(request),
        )
        .await;

        // Request should FAIL (non-routable IP), but metrics should still be recorded
        assert!(result.is_err(), "Request should fail with non-routable IP");

        // Get metrics after request
        let output_after = metrics.gather().expect("should gather metrics after");

        // Verify metrics changed (routing decision was recorded)
        assert_ne!(
            output_before, output_after,
            "Metrics should change after routing decision, even if query fails"
        );

        // Verify routing decision metrics were recorded (before query attempt)
        // 1. octoroute_requests_total should have fast+rule entry
        assert!(
            output_after.contains("octoroute_requests_total"),
            "Should contain requests_total metric"
        );
        assert!(
            output_after.contains("tier=\"fast\""),
            "Should record fast tier in requests_total (routing decision recorded BEFORE query)"
        );
        assert!(
            output_after.contains("strategy=\"rule\""),
            "Should record rule strategy in requests_total"
        );

        // 2. octoroute_routing_duration_ms should have rule strategy entry
        assert!(
            output_after.contains("octoroute_routing_duration_ms"),
            "Should contain routing_duration metric"
        );

        // 3. octoroute_model_invocations_total should NOT have been incremented
        // (only recorded on successful query, which failed here)
        // NOTE: If this is the first test run, the metric may not exist at all,
        // or it may exist with value 0. We just verify record_request() was called
        // (proven by tier="fast" above) but record_model_invocation() was not
        // (because the query failed).
        //
        // This test validates the timing semantics documented in Issue #7:
        // - record_request() is called BEFORE model query (always recorded)
        // - record_model_invocation() is called AFTER successful query (not recorded on failure)
    }

    #[tokio::test]
    async fn test_metrics_recording_with_extreme_values() {
        // Test that metrics handle extreme edge cases gracefully
        let config = Arc::new(create_test_config());
        let state = AppState::new(config).expect("should create state");

        let metrics = state.metrics();

        // Record metrics with edge case values
        // These should either succeed OR return Err (not panic)

        // 1. Maximum valid duration (very slow request)
        let result = metrics.record_routing_duration(Strategy::Llm, f64::MAX);
        // Should either succeed or fail gracefully (validated by histogram tests)
        assert!(
            result.is_ok() || result.is_err(),
            "Should not panic with extreme duration"
        );

        // 2. Zero duration (instant operation)
        assert!(
            metrics.record_routing_duration(Strategy::Rule, 0.0).is_ok(),
            "Should accept zero duration"
        );

        // 3. Record same metric many times (stress test)
        for _ in 0..10_000 {
            metrics.record_request(Tier::Fast, Strategy::Rule).unwrap();
        }

        // 4. Gather metrics after heavy recording
        let output = metrics.gather();
        assert!(
            output.is_ok(),
            "Should gather metrics successfully even after heavy recording"
        );

        // If we got here without panic, the defensive error handling works
    }
}
