//! Request ID middleware for distributed tracing
//!
//! Generates a unique UUID for each incoming request and makes it available
//! throughout the request lifecycle via Axum extensions.

use axum::{extract::Request, http::HeaderValue, middleware::Next, response::Response};
use uuid::Uuid;

/// Request ID header name
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Request ID wrapper type for Axum extensions
#[derive(Debug, Clone, Copy)]
pub struct RequestId(pub Uuid);

impl RequestId {
    /// Generate a new random request ID
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Get the UUID value
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    /// Get the string representation
    pub fn as_str(&self) -> String {
        self.0.to_string()
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Middleware that generates and attaches a request ID to each request
///
/// The request ID is:
/// 1. Generated as a UUID v4
/// 2. Attached to the request via extensions (accessible in handlers)
/// 3. Added to the response headers for client correlation
pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    // Generate new request ID
    let request_id = RequestId::new();

    // Log the incoming request with ID
    tracing::debug!(
        request_id = %request_id,
        method = %request.method(),
        uri = %request.uri(),
        "Incoming request"
    );

    // Insert request ID into request extensions
    request.extensions_mut().insert(request_id);

    // Process the request
    let mut response = next.run(request).await;

    // Add request ID to response headers for client correlation
    if let Ok(header_value) = HeaderValue::from_str(&request_id.to_string()) {
        response
            .headers_mut()
            .insert(REQUEST_ID_HEADER, header_value);
    }

    response
}
