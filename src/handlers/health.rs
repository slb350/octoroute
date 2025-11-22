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
}

impl HealthResponse {
    /// Construct a new HealthResponse from health tracking failure count
    ///
    /// # Arguments
    /// * `health_tracking_failures` - Number of health tracking failures (from metrics)
    ///
    /// # Returns
    /// A HealthResponse with:
    /// - status: Always HealthStatus::Ok
    /// - health_tracking_status: Degraded if failures > 0, otherwise Operational
    pub fn new(health_tracking_failures: u64) -> Self {
        let health_tracking_status = if health_tracking_failures > 0 {
            HealthTrackingStatus::Degraded
        } else {
            HealthTrackingStatus::Operational
        };

        Self {
            status: HealthStatus::Ok,
            health_tracking_status,
        }
    }
}

/// Health check handler
///
/// Returns 200 OK with health status and health tracking status.
///
/// Health tracking status is "degraded" if any health tracking failures have occurred,
/// otherwise "operational". This indicates whether mark_success/mark_failure operations
/// are functioning correctly.
pub async fn handler(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let health_tracking_failures = state.metrics().health_tracking_failures_count();
    let response = HealthResponse::new(health_tracking_failures);

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
    }

    #[tokio::test]
    async fn test_health_handler_shows_degraded_when_failures_occur() {
        let state = create_test_state();

        // Increment health_tracking_failures metric
        state.metrics().health_tracking_failure();

        let (status, Json(response)) = handler(State(state)).await;

        assert_eq!(status, StatusCode::OK);

        // Verify serialization produces expected JSON
        let json = serde_json::to_value(&response).expect("Should serialize");
        assert_eq!(json["status"], "OK");
        assert_eq!(json["health_tracking_status"], "degraded");
    }
}
