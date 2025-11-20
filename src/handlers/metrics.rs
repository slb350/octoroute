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
    match state.metrics() {
        Some(metrics) => match metrics.gather() {
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
        },
        None => {
            tracing::warn!(
                "Metrics endpoint accessed but metrics feature not enabled. \
                Returning 404. Build with --features metrics to enable."
            );
            (
                StatusCode::NOT_FOUND,
                "Metrics not enabled. Build with --features metrics".to_string(),
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

        // Record some metrics if available
        if let Some(metrics) = state.metrics() {
            metrics.record_request("fast", "rule").unwrap();
        }

        let (status, body) = handler(State(state)).await;

        #[cfg(feature = "metrics")]
        {
            assert_eq!(status, StatusCode::OK);
            assert!(body.contains("# HELP"));
            assert!(body.contains("# TYPE"));
        }

        #[cfg(not(feature = "metrics"))]
        {
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(body.contains("Metrics not enabled"));
        }
    }

    #[tokio::test]
    async fn test_metrics_handler_without_feature_returns_404() {
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

        let (status, _body) = handler(State(state)).await;

        #[cfg(not(feature = "metrics"))]
        {
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(_body.contains("Metrics not enabled"));
        }

        #[cfg(feature = "metrics")]
        {
            // When metrics are enabled, should return OK even with no data
            assert_eq!(status, StatusCode::OK);
        }
    }
}
