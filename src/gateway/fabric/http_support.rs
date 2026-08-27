//! Shared HTTP limits, error bodies, and response hardening for the v3 runtime.

use axum::{
    Json,
    body::{Body, BodyDataStream},
    http::{
        HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode,
        header::{CONTENT_TYPE, WWW_AUTHENTICATE},
    },
    middleware::Next,
    response::IntoResponse,
};
use futures::Stream;
use serde_json::json;
use std::{
    pin::Pin,
    sync::Mutex,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::OwnedSemaphorePermit;
use uuid::Uuid;

pub(super) const REQUEST_ID_HEADER: &str = "x-request-id";
pub(super) const OCTOROUTE_REQUEST_ID_HEADER: &str = "x-octoroute-request-id";

/// Authentication and header-bound failures for protected metadata routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetadataAuthorizationError {
    /// Aggregate header bytes exceed the configured limit.
    HeadersTooLarge,
    /// Bearer authentication failed.
    Unauthorized,
}

pub(super) fn header_bytes(headers: &HeaderMap) -> usize {
    headers.iter().fold(0usize, |total, (name, value)| {
        total
            .saturating_add(name.as_str().len())
            .saturating_add(value.as_bytes().len())
            .saturating_add(4)
    })
}

pub(super) fn hold_response_guard(
    response: Response<Body>,
    guard: OwnedSemaphorePermit,
) -> Response<Body> {
    let (parts, body) = response.into_parts();
    let stream = GuardedResponseBody {
        inner: body.into_data_stream(),
        _guard: guard,
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

struct GuardedResponseBody {
    inner: BodyDataStream,
    _guard: OwnedSemaphorePermit,
}

impl Stream for GuardedResponseBody {
    type Item = Result<bytes::Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(context)
    }
}

pub(super) struct FixedWindowRateLimiter {
    limit: u32,
    state: Mutex<RateWindow>,
}

struct RateWindow {
    started: Instant,
    requests: u32,
}

impl FixedWindowRateLimiter {
    pub(super) fn new(limit: u32) -> Self {
        Self {
            limit,
            state: Mutex::new(RateWindow {
                started: Instant::now(),
                requests: 0,
            }),
        }
    }

    pub(super) fn allow(&self) -> bool {
        let mut state = self.state.lock().expect("rate limiter mutex poisoned");
        if state.started.elapsed() >= Duration::from_secs(60) {
            state.started = Instant::now();
            state.requests = 0;
        }
        if state.requests >= self.limit {
            false
        } else {
            state.requests += 1;
            true
        }
    }
}

pub(super) fn metadata_authorization_error(
    error: MetadataAuthorizationError,
    request_id: &str,
) -> Response<Body> {
    let (status, message, error_type, code) = match error {
        MetadataAuthorizationError::HeadersTooLarge => (
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "request headers exceed the configured size limit",
            "invalid_request_error",
            "headers_too_large",
        ),
        MetadataAuthorizationError::Unauthorized => (
            StatusCode::UNAUTHORIZED,
            "bearer authentication failed",
            "authentication_error",
            "authentication_error",
        ),
    };
    let mut response = error_response(status, message, error_type, code, request_id);
    if error == MetadataAuthorizationError::Unauthorized {
        response
            .headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}

pub(super) fn error_response(
    status: StatusCode,
    message: &str,
    error_type: &str,
    code: &str,
    request_id: &str,
) -> Response<Body> {
    let mut response = (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": error_type,
                "code": code
            }
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    insert_header(response.headers_mut(), REQUEST_ID_HEADER, request_id);
    insert_header(
        response.headers_mut(),
        OCTOROUTE_REQUEST_ID_HEADER,
        request_id,
    );
    response
}

pub(super) fn rate_limit_response(message: &str, code: &str, request_id: &str) -> Response<Body> {
    let mut response = error_response(
        StatusCode::TOO_MANY_REQUESTS,
        message,
        "rate_limit_error",
        code,
        request_id,
    );
    response
        .headers_mut()
        .insert("retry-after", HeaderValue::from_static("60"));
    response
}

pub(super) fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    let name = HeaderName::from_static(name);
    let value =
        HeaderValue::from_str(value).expect("configuration produced an invalid HTTP header");
    headers.insert(name, value);
}

pub(super) async fn security_headers(request: Request<Body>, next: Next) -> Response<Body> {
    let mut response = next.run(request).await;
    let gateway_request_id = response
        .headers()
        .get(OCTOROUTE_REQUEST_ID_HEADER)
        .cloned()
        .unwrap_or_else(|| {
            HeaderValue::from_str(&Uuid::new_v4().to_string()).expect("UUID is a valid header")
        });
    if !response.headers().contains_key(OCTOROUTE_REQUEST_ID_HEADER) {
        response
            .headers_mut()
            .insert(OCTOROUTE_REQUEST_ID_HEADER, gateway_request_id.clone());
    }
    if !response.headers().contains_key(REQUEST_ID_HEADER) {
        response
            .headers_mut()
            .insert(REQUEST_ID_HEADER, gateway_request_id);
    }
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        (
            "permissions-policy",
            "camera=(), microphone=(), geolocation=(), payment=()",
        ),
        (
            "content-security-policy",
            "default-src 'none'; frame-ancestors 'none'",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}
