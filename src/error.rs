//! Error types for Octoroute
//!
//! All errors implement `IntoResponse` for Axum handlers.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

use crate::router::llm_based::LlmRouterError;

/// Main error type for the application
#[derive(Error, Debug)]
pub enum AppError {
    /// Generic configuration error (deprecated - use specific variants below)
    #[error("Configuration error: {0}")]
    Config(String),

    /// Failed to read config file from filesystem
    #[error("Failed to read config file '{path}': {source}")]
    ConfigFileRead {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Failed to parse TOML configuration
    #[error("Failed to parse config file '{path}': {source}")]
    ConfigParseFailed {
        path: String,
        #[source]
        source: toml::de::Error,
    },

    /// Config validation failed after successful parsing
    #[error("Config validation failed for '{path}': {reason}")]
    ConfigValidationFailed { path: String, reason: String },

    #[error("Invalid request: {0}")]
    Validation(String),

    #[error("Routing failed: {0}")]
    RoutingFailed(String),

    #[error(
        "Stream interrupted from {endpoint} after receiving {bytes_received} bytes ({blocks_received} blocks)"
    )]
    StreamInterrupted {
        endpoint: String,
        bytes_received: usize,
        blocks_received: usize,
    },

    #[error("Request to {endpoint} timed out after {timeout_seconds} seconds")]
    EndpointTimeout {
        endpoint: String,
        timeout_seconds: u64,
    },

    #[error("Health check failed for {endpoint}: {reason}")]
    HealthCheckFailed { endpoint: String, reason: String },

    #[error("Failed to query model at {endpoint}: {reason}")]
    ModelQueryFailed { endpoint: String, reason: String },

    #[error(transparent)]
    LlmRouting(#[from] LlmRouterError),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::Validation(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            Self::Config(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            Self::ConfigFileRead { .. } => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            Self::ConfigParseFailed { .. } => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            Self::ConfigValidationFailed { .. } => {
                (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
            }
            Self::RoutingFailed(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
            Self::StreamInterrupted { .. } => (StatusCode::BAD_GATEWAY, self.to_string()),
            Self::EndpointTimeout { .. } => (StatusCode::GATEWAY_TIMEOUT, self.to_string()),
            Self::HealthCheckFailed { .. } => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            Self::ModelQueryFailed { .. } => (StatusCode::BAD_GATEWAY, self.to_string()),
            Self::LlmRouting(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            Self::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg.clone()),
        };

        let body = Json(serde_json::json!({
            "error": message,
        }));

        (status, body).into_response()
    }
}

/// Convenience type alias for Results
pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_error_creates() {
        let err = AppError::Config("test error".to_string());
        assert_eq!(err.to_string(), "Configuration error: test error");
    }

    #[test]
    fn test_validation_error_creates() {
        let err = AppError::Validation("invalid input".to_string());
        assert_eq!(err.to_string(), "Invalid request: invalid input");
    }

    #[test]
    fn test_routing_failed_error_creates() {
        let err = AppError::RoutingFailed("no rules matched".to_string());
        assert_eq!(err.to_string(), "Routing failed: no rules matched");
    }

    #[test]
    fn test_internal_error_creates() {
        let err = AppError::Internal("unexpected state".to_string());
        assert_eq!(err.to_string(), "Internal error: unexpected state");
    }

    #[test]
    fn test_config_error_response_status() {
        let err = AppError::Config("test".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_validation_error_response_status() {
        let err = AppError::Validation("test".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_routing_failed_error_response_status() {
        let err = AppError::RoutingFailed("test".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_internal_error_response_status() {
        let err = AppError::Internal("test".to_string());
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_stream_interrupted_error_returns_502_bad_gateway() {
        let err = AppError::StreamInterrupted {
            endpoint: "http://localhost:1234/v1".to_string(),
            bytes_received: 1024,
            blocks_received: 5,
        };
        let response = err.into_response();
        assert_eq!(
            response.status(),
            StatusCode::BAD_GATEWAY,
            "StreamInterrupted must return 502 BAD_GATEWAY to indicate upstream server failure"
        );
    }

    #[test]
    fn test_endpoint_timeout_error_returns_504_gateway_timeout() {
        let err = AppError::EndpointTimeout {
            endpoint: "http://localhost:1234/v1".to_string(),
            timeout_seconds: 30,
        };
        let response = err.into_response();
        assert_eq!(
            response.status(),
            StatusCode::GATEWAY_TIMEOUT,
            "EndpointTimeout must return 504 GATEWAY_TIMEOUT to distinguish from stream failures"
        );
    }

    #[test]
    fn test_model_query_failed_error_returns_502_bad_gateway() {
        let err = AppError::ModelQueryFailed {
            endpoint: "http://localhost:1234/v1".to_string(),
            reason: "connection refused".to_string(),
        };
        let response = err.into_response();
        assert_eq!(
            response.status(),
            StatusCode::BAD_GATEWAY,
            "ModelQueryFailed must return 502 BAD_GATEWAY to indicate upstream server failure"
        );
    }
}
