//! Health check endpoint
//!
//! Provides a simple health check for monitoring and load balancers.

use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::handlers::AppState;

/// Service health status
///
/// Type-safe enum preventing invalid status values at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum HealthStatus {
    /// Service is operational
    #[serde(rename = "OK")]
    Ok,
}

/// Health tracking system status
///
/// Indicates whether mark_success/mark_failure operations are functioning correctly.
/// Type-safe enum preventing invalid values at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthTrackingStatus {
    /// Health tracking is functioning correctly (no failures)
    Operational,
    /// Health tracking has encountered failures (degraded functionality)
    Degraded,
}

/// Health check response
///
/// Uses type-safe enums for status fields, preventing invalid states at compile time.
/// Fields are private to enforce construction through the `new()` method.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Service status (always OK)
    status: HealthStatus,
    /// Health tracking status: operational or degraded
    health_tracking_status: HealthTrackingStatus,
    /// Metrics recording status: operational or degraded
    metrics_recording_status: HealthTrackingStatus,
    /// Background health task status: operational or degraded
    background_task_status: HealthTrackingStatus,
    /// Number of background task failures (restart attempts)
    background_task_failures: u64,
}

impl HealthResponse {
    /// Construct a new HealthResponse from failure counts
    ///
    /// # Arguments
    /// * `health_tracking_failures` - Number of health tracking failures (from metrics)
    /// * `metrics_recording_failures` - Number of metrics recording failures (from metrics)
    /// * `background_task_failures` - Number of background task failures/restarts (from metrics)
    ///
    /// # Returns
    /// A HealthResponse with:
    /// - status: Always HealthStatus::Ok
    /// - health_tracking_status: Degraded if failures > 0, otherwise Operational
    /// - metrics_recording_status: Degraded if failures > 0, otherwise Operational
    /// - background_task_status: Degraded if failures > 0, otherwise Operational
    /// - background_task_failures: Count of failures for operator visibility
    pub fn new(
        health_tracking_failures: u64,
        metrics_recording_failures: u64,
        background_task_failures: u64,
    ) -> Self {
        let health_tracking_status = if health_tracking_failures > 0 {
            HealthTrackingStatus::Degraded
        } else {
            HealthTrackingStatus::Operational
        };

        let metrics_recording_status = if metrics_recording_failures > 0 {
            HealthTrackingStatus::Degraded
        } else {
            HealthTrackingStatus::Operational
        };

        let background_task_status = if background_task_failures > 0 {
            HealthTrackingStatus::Degraded
        } else {
            HealthTrackingStatus::Operational
        };

        Self {
            status: HealthStatus::Ok,
            health_tracking_status,
            metrics_recording_status,
            background_task_status,
            background_task_failures,
        }
    }
}

/// Health check handler
///
/// Returns 200 OK with health status, health tracking status, and metrics recording status.
///
/// - Health tracking status is "degraded" if any health tracking failures have occurred,
///   indicating mark_success/mark_failure operations are failing.
/// - Metrics recording status is "degraded" if any metrics recording failures have occurred,
///   indicating Prometheus metrics recording is failing.
pub async fn handler(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let health_tracking_failures = state.metrics().health_tracking_failures_count();
    let metrics_recording_failures = state.metrics().metrics_recording_failures_count();
    let background_task_failures = state.metrics().background_task_failures_count();
    let response = HealthResponse::new(
        health_tracking_failures,
        metrics_recording_failures,
        background_task_failures,
    );

    (StatusCode::OK, Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use axum::extract::State;
    use std::sync::Arc;

    fn create_test_state() -> AppState {
        let toml = r#"
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
router_tier = "balanced"
"#;
        let config: Config = toml::from_str(toml).expect("should parse test config");
        AppState::new(Arc::new(config)).expect("should create AppState")
    }

    #[tokio::test]
    async fn test_health_handler_returns_ok() {
        let state = create_test_state();
        let (status, Json(response)) = handler(State(state)).await;

        assert_eq!(status, StatusCode::OK);

        // Verify serialization produces expected JSON
        let json = serde_json::to_value(&response).expect("Should serialize");
        assert_eq!(json["status"], "OK");
        assert_eq!(json["health_tracking_status"], "operational");
        assert_eq!(json["metrics_recording_status"], "operational");
    }

    #[tokio::test]
    async fn test_health_handler_shows_degraded_when_health_tracking_fails() {
        let state = create_test_state();

        // Increment health_tracking_failures metric with test labels
        state
            .metrics()
            .health_tracking_failure("test-endpoint", "unknown_endpoint");

        let (status, Json(response)) = handler(State(state)).await;

        assert_eq!(status, StatusCode::OK);

        // Verify serialization produces expected JSON
        let json = serde_json::to_value(&response).expect("Should serialize");
        assert_eq!(json["status"], "OK");
        assert_eq!(json["health_tracking_status"], "degraded");
        assert_eq!(json["metrics_recording_status"], "operational");
    }

    #[tokio::test]
    async fn test_health_handler_shows_degraded_when_metrics_recording_fails() {
        let state = create_test_state();

        // Increment metrics_recording_failures metric with test label
        state.metrics().metrics_recording_failure("record_request");

        let (status, Json(response)) = handler(State(state)).await;

        assert_eq!(status, StatusCode::OK);

        // Verify serialization produces expected JSON
        let json = serde_json::to_value(&response).expect("Should serialize");
        assert_eq!(json["status"], "OK");
        assert_eq!(json["health_tracking_status"], "operational");
        assert_eq!(json["metrics_recording_status"], "degraded");
    }

    #[tokio::test]
    async fn test_health_handler_shows_both_degraded_when_both_fail() {
        let state = create_test_state();

        // Increment both failure metrics with test labels
        state
            .metrics()
            .health_tracking_failure("test-endpoint", "unknown_endpoint");
        state.metrics().metrics_recording_failure("record_request");

        let (status, Json(response)) = handler(State(state)).await;

        assert_eq!(status, StatusCode::OK);

        // Verify serialization produces expected JSON
        let json = serde_json::to_value(&response).expect("Should serialize");
        assert_eq!(json["status"], "OK");
        assert_eq!(json["health_tracking_status"], "degraded");
        assert_eq!(json["metrics_recording_status"], "degraded");
    }
}
