//! Health check endpoint
//!
//! Provides a simple health check for monitoring and load balancers.

use axum::http::StatusCode;

/// Health check handler
///
/// Returns 200 OK to indicate the service is running.
///
/// # Examples
///
/// ```
/// # use octoroute::handlers::health::handler;
/// # tokio_test::block_on(async {
/// let response = handler().await;
/// assert_eq!(response.0, axum::http::StatusCode::OK);
/// # });
/// ```
pub async fn handler() -> (StatusCode, &'static str) {
    (StatusCode::OK, "OK")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health_handler_returns_ok() {
        let (status, body) = handler().await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "OK");
    }
}
