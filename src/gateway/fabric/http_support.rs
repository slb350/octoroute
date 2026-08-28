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

/// Process-global fixed-window request limiter.
///
/// The semantics are deliberately coarse, and worth stating exactly:
///
/// - It is **one counter for the whole gateway**, not one per caller. Every
///   client authenticates with the same configured bearer, so Octoroute has no
///   client identity to key a bucket on; the limit is a bound on total inbound
///   work, and one busy caller can consume the whole window and make every
///   other caller wait for it.
/// - The window is **fixed, not sliding**: the counter resets 60s after the
///   window opened. A caller that spends the limit at the end of one window and
///   again at the start of the next sends up to twice the limit within a single
///   60-second span.
///
/// Neither is a bug to route around here: per-caller fairness needs per-caller
/// credentials, which the configuration does not have.
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
        // Recover rather than panic: the guarded state is two plain counters, a
        // poisoned lock leaves them consistent, and panicking here would turn
        // one unrelated panic into a failure of every subsequent request. The
        // metrics registry already recovers for the same reason.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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

/// Set one gateway-owned response header, or set nothing at all.
///
/// Every value here comes from validated configuration or a generated id, so an
/// invalid one is unreachable today. It is unreachable by validator discipline
/// alone, though, and a looser validator later would turn this into a panic on
/// the request path. Dropping the header degrades a diagnostic; panicking loses
/// the response.
pub(super) fn insert_header(headers: &mut HeaderMap, name: &'static str, value: &str) {
    match HeaderValue::from_str(value) {
        Ok(value) => {
            headers.insert(HeaderName::from_static(name), value);
        }
        Err(_) => tracing::warn!(header = name, "refusing to set an invalid response header"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use tower::ServiceExt as _;

    async fn response_with_distinct_request_ids() -> Response<Body> {
        let mut response = Response::new(Body::empty());
        response.headers_mut().insert(
            OCTOROUTE_REQUEST_ID_HEADER,
            HeaderValue::from_static("gateway-id"),
        );
        response.headers_mut().insert(
            REQUEST_ID_HEADER,
            HeaderValue::from_static("application-id"),
        );
        response
    }

    #[tokio::test]
    async fn security_headers_generate_matching_request_ids_when_both_are_absent() {
        let app = Router::new()
            .route("/", get(|| async { Response::new(Body::empty()) }))
            .layer(axum::middleware::from_fn(security_headers));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        let gateway_id = response
            .headers()
            .get(OCTOROUTE_REQUEST_ID_HEADER)
            .expect("gateway request id");
        assert_eq!(response.headers().get(REQUEST_ID_HEADER), Some(gateway_id));
        Uuid::parse_str(gateway_id.to_str().expect("ASCII request id")).expect("UUID request id");
    }

    #[tokio::test]
    async fn security_headers_preserve_an_existing_application_request_id() {
        let app = Router::new()
            .route("/", get(response_with_distinct_request_ids))
            .layer(axum::middleware::from_fn(security_headers));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(
            response.headers().get(OCTOROUTE_REQUEST_ID_HEADER),
            Some(&HeaderValue::from_static("gateway-id"))
        );
        assert_eq!(
            response.headers().get(REQUEST_ID_HEADER),
            Some(&HeaderValue::from_static("application-id"))
        );
    }

    /// A header value that cannot be represented must cost the header, not the
    /// response. The values are validated upstream today, so this pins the
    /// behaviour a future looser validator would otherwise turn into a panic.
    #[test]
    fn an_invalid_header_value_is_dropped_rather_than_panicking() {
        let mut headers = HeaderMap::new();
        insert_header(&mut headers, REQUEST_ID_HEADER, "line-one\nline-two");
        assert!(headers.get(REQUEST_ID_HEADER).is_none());

        insert_header(&mut headers, REQUEST_ID_HEADER, "request-1");
        assert_eq!(headers.get(REQUEST_ID_HEADER).expect("header"), "request-1");
    }

    /// The window is fixed and process-global: the limit applies to the gateway
    /// as a whole and resets only when the window rolls over.
    #[test]
    fn the_window_bounds_total_requests_until_it_rolls_over() {
        let limiter = FixedWindowRateLimiter::new(2);
        assert!(limiter.allow());
        assert!(limiter.allow());
        assert!(!limiter.allow());
    }
}
