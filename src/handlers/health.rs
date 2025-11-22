//! Health check endpoint
//!
//! Provides a simple health check for monitoring and load balancers.

use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::handlers::AppState;

/// Health check response
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Service status
    pub status: &'static str,
    /// Health tracking status: "operational" or "degraded"
    pub health_tracking_status: &'static str,
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
    let health_tracking_status = if health_tracking_failures > 0 {
        "degraded"
    } else {
        "operational"
    };

    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "OK",
            health_tracking_status,
        }),
    )
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
        let (status, Json(body)) = handler(State(state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.status, "OK");
        assert_eq!(body.health_tracking_status, "operational");
    }

    #[tokio::test]
    async fn test_health_handler_shows_degraded_when_failures_occur() {
        let state = create_test_state();

        // Increment health_tracking_failures metric
        state.metrics().health_tracking_failure();

        let (status, Json(body)) = handler(State(state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.status, "OK");
        assert_eq!(body.health_tracking_status, "degraded");
    }
}
