//! Prometheus metrics endpoint
//!
//! Exposes metrics in Prometheus text format for scraping.
//!
//! This module is only available when the `metrics` feature is enabled.

use axum::{extract::State, http::StatusCode};

use crate::handlers::AppState;

/// Metrics handler for Prometheus scraping
///
/// Returns metrics in Prometheus text format.
///
/// # Response
///
/// - `200 OK` with metrics in Prometheus text format
/// - `500 Internal Server Error` if metrics collection fails
///
/// # Example
///
/// ```bash
/// curl http://localhost:3000/metrics
/// # HELP octoroute_requests_total Total number of chat requests
/// # TYPE octoroute_requests_total counter
/// octoroute_requests_total{tier="fast",strategy="rule"} 42
/// ```
pub async fn handler(State(state): State<AppState>) -> (StatusCode, String) {
    let metrics = state.metrics();
    match metrics.gather() {
        Ok(output) => (StatusCode::OK, output),
        Err(e) => {
            tracing::error!(
                error = %e,
                "Failed to gather metrics for Prometheus scraping. \
                This indicates a metrics encoding issue (invalid UTF-8, \
                corrupted labels, or encoder failure). Error: {}",
                e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to gather metrics: {}", e),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_metrics_handler_returns_prometheus_format() {
        let config_str = r#"
            [server]
            host = "127.0.0.1"
            port = 3000

            [[models.fast]]
            name = "test-8b"
            base_url = "http://localhost:11434/v1"
            max_tokens = 4096
            temperature = 0.7
            weight = 1.0
            priority = 1

            [[models.balanced]]
            name = "test-30b"
            base_url = "http://localhost:1234/v1"
            max_tokens = 8192
            temperature = 0.7
            weight = 1.0
            priority = 1

            [[models.deep]]
            name = "test-120b"
            base_url = "http://localhost:8080/v1"
            max_tokens = 16384
            temperature = 0.7
            weight = 1.0
            priority = 1

            [routing]
            strategy = "hybrid"
            router_model = "balanced"
        "#;

        let config: Config = toml::from_str(config_str).unwrap();
        let state = AppState::new(Arc::new(config)).unwrap();

        // Record some metrics
        let metrics = state.metrics();
        metrics
            .record_request(crate::metrics::Tier::Fast, crate::metrics::Strategy::Rule)
            .unwrap();

        let (status, body) = handler(State(state)).await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("# HELP"));
        assert!(body.contains("# TYPE"));
    }

    // ===== GAP #5 Fix: /metrics Endpoint Error Cases =====
    #[tokio::test]
    async fn test_concurrent_metrics_scraping() {
        use std::sync::Arc;
        use tokio::task;

        let config_str = r#"
            [server]
            host = "127.0.0.1"
            port = 3000

            [[models.fast]]
            name = "test-8b"
            base_url = "http://localhost:11434/v1"
            max_tokens = 4096
            temperature = 0.7
            weight = 1.0
            priority = 1

            [[models.balanced]]
            name = "test-balanced"
            base_url = "http://localhost:1235/v1"
            max_tokens = 8192
            temperature = 0.7
            weight = 1.0
            priority = 1

            [[models.deep]]
            name = "test-deep"
            base_url = "http://localhost:1236/v1"
            max_tokens = 16384
            temperature = 0.7
            weight = 1.0
            priority = 1

            [routing]
            strategy = "rule"
            router_model = "balanced"
        "#;

        let config: Config = toml::from_str(config_str).unwrap();
        let state = Arc::new(AppState::new(Arc::new(config)).unwrap());

        // Record some metrics
        let metrics = state.metrics();
        for i in 0..100 {
            let tier = match i % 3 {
                0 => crate::metrics::Tier::Fast,
                1 => crate::metrics::Tier::Balanced,
                _ => crate::metrics::Tier::Deep,
            };
            let strategy = if i % 2 == 0 {
                crate::metrics::Strategy::Rule
            } else {
                crate::metrics::Strategy::Llm
            };
            metrics.record_request(tier, strategy).unwrap();
        }

        // Spawn 10 concurrent scraping requests
        let mut handles = vec![];
        for _ in 0..10 {
            let state_clone = Arc::clone(&state);
            let handle = task::spawn(async move {
                let (status, body) = handler(State(state_clone.as_ref().clone())).await;
                (status, body)
            });
            handles.push(handle);
        }

        // Wait for all requests to complete
        let results: Vec<_> = futures::future::join_all(handles).await;

        // Verify all requests succeeded
        for (idx, result) in results.iter().enumerate() {
            let (status, body) = result.as_ref().unwrap();
            assert_eq!(
                *status,
                StatusCode::OK,
                "Request {} should succeed during concurrent scraping",
                idx
            );
            assert!(
                body.contains("octoroute_requests_total"),
                "Request {} should return valid metrics",
                idx
            );
        }

        // All responses should be identical (deterministic scraping)
        let first_body = &results[0].as_ref().unwrap().1;
        for (idx, result) in results.iter().enumerate().skip(1) {
            let body = &result.as_ref().unwrap().1;
            assert_eq!(
                body, first_body,
                "Concurrent scraping should return identical results (request {})",
                idx
            );
        }
    }

    #[tokio::test]
    async fn test_metrics_output_valid_prometheus_format() {
        let config_str = r#"
            [server]
            host = "127.0.0.1"
            port = 3000

            [[models.fast]]
            name = "test-8b"
            base_url = "http://localhost:11434/v1"
            max_tokens = 4096
            temperature = 0.7
            weight = 1.0
            priority = 1

            [[models.balanced]]
            name = "test-balanced"
            base_url = "http://localhost:1235/v1"
            max_tokens = 8192
            temperature = 0.7
            weight = 1.0
            priority = 1

            [[models.deep]]
            name = "test-deep"
            base_url = "http://localhost:1236/v1"
            max_tokens = 16384
            temperature = 0.7
            weight = 1.0
            priority = 1

            [routing]
            strategy = "rule"
            router_model = "balanced"
        "#;

        let config: Config = toml::from_str(config_str).unwrap();
        let state = AppState::new(Arc::new(config)).unwrap();

        // Record various metrics
        let metrics = state.metrics();
        metrics
            .record_request(crate::metrics::Tier::Fast, crate::metrics::Strategy::Rule)
            .unwrap();
        metrics
            .record_routing_duration(crate::metrics::Strategy::Rule, 1.5)
            .unwrap();
        metrics
            .record_model_invocation(crate::metrics::Tier::Fast)
            .unwrap();

        let (status, body) = handler(State(state)).await;

        assert_eq!(status, StatusCode::OK);

        // Validate Prometheus text format structure
        // Format spec: https://prometheus.io/docs/instrumenting/exposition_formats/

        // 1. Should have HELP lines (metric documentation)
        assert!(
            body.contains("# HELP octoroute_requests_total"),
            "Should have HELP for requests_total"
        );
        assert!(
            body.contains("# HELP octoroute_routing_duration_ms"),
            "Should have HELP for routing_duration"
        );
        assert!(
            body.contains("# HELP octoroute_model_invocations_total"),
            "Should have HELP for model_invocations"
        );

        // 2. Should have TYPE lines (metric type declaration)
        assert!(
            body.contains("# TYPE octoroute_requests_total counter"),
            "requests_total should be counter type"
        );
        assert!(
            body.contains("# TYPE octoroute_routing_duration_ms histogram"),
            "routing_duration should be histogram type"
        );
        assert!(
            body.contains("# TYPE octoroute_model_invocations_total counter"),
            "model_invocations should be counter type"
        );

        // 3. Should have metric lines with labels and values
        // Format: metric_name{label1="value1",label2="value2"} numeric_value
        let metric_lines: Vec<&str> = body
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .collect();

        assert!(
            !metric_lines.is_empty(),
            "Should have at least one metric value line"
        );

        // 4. Validate metric line format
        for line in &metric_lines {
            // Should have metric name
            assert!(
                line.contains("octoroute_"),
                "Metric line should start with metric name: {}",
                line
            );

            // If has labels, should have valid label format
            if line.contains('{') {
                assert!(line.contains('}'), "Labels should be closed: {}", line);
                assert!(
                    line.contains('='),
                    "Labels should have key=value pairs: {}",
                    line
                );
                assert!(
                    line.contains('"'),
                    "Label values should be quoted: {}",
                    line
                );
            }

            // Should end with numeric value (or +Inf for histogram buckets)
            let last_token = line.split_whitespace().last().unwrap();
            assert!(
                last_token.parse::<f64>().is_ok()
                    || last_token == "+Inf"
                    || last_token == "-Inf"
                    || last_token == "NaN",
                "Should end with numeric value or special float: {} (line: {})",
                last_token,
                line
            );
        }

        // 5. Verify no duplicate metric lines (each label combination should be unique)
        let mut seen = std::collections::HashSet::new();
        for line in metric_lines {
            if let Some(metric_part) = line.split_whitespace().next() {
                assert!(
                    seen.insert(metric_part),
                    "Duplicate metric line detected: {}",
                    metric_part
                );
            }
        }
    }

    #[tokio::test]
    async fn test_metrics_handler_with_empty_registry() {
        // Test that handler works even when no metrics have been recorded
        let config_str = r#"
            [server]
            host = "127.0.0.1"
            port = 3000

            [[models.fast]]
            name = "test-8b"
            base_url = "http://localhost:11434/v1"
            max_tokens = 4096
            temperature = 0.7
            weight = 1.0
            priority = 1

            [[models.balanced]]
            name = "test-balanced"
            base_url = "http://localhost:1235/v1"
            max_tokens = 8192
            temperature = 0.7
            weight = 1.0
            priority = 1

            [[models.deep]]
            name = "test-deep"
            base_url = "http://localhost:1236/v1"
            max_tokens = 16384
            temperature = 0.7
            weight = 1.0
            priority = 1

            [routing]
            strategy = "rule"
            router_model = "balanced"
        "#;

        let config: Config = toml::from_str(config_str).unwrap();
        let state = AppState::new(Arc::new(config)).unwrap();

        // Don't record any metrics - test with empty registry

        let (status, body) = handler(State(state)).await;

        assert_eq!(status, StatusCode::OK, "Should succeed with empty registry");
        assert!(
            body.contains("# HELP") || body.is_empty(),
            "Should return valid output even with no data"
        );
    }
}
