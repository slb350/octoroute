//! The preflight surface: authenticate, bound the headers, bound the body, and
//! bound concurrency - in that order.
//!
//! The order is the contract ("authenticate before reading the request body"),
//! so it is asserted as an observable fact rather than by reading the source:
//! an unauthenticated oversized body is refused without its stream ever being
//! polled.

use super::*;
use crate::gateway::fabric::FabricTransport;
use std::sync::atomic::{AtomicBool, Ordering};

/// The shipped configuration, whose upstreams are unreachable on purpose: every
/// test in this module is refused before any route step runs.
fn preflight_config() -> FabricConfig {
    FabricConfig::from_toml(include_str!("../../../../config.toml")).expect("config")
}

fn preflight_service(config: FabricConfig) -> FabricGatewayService<FabricTransport> {
    FabricGatewayService::from_config(
        config,
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service")
}

/// A request body that records whether anything ever read it.
fn observed_body(bytes: usize) -> (Body, Arc<AtomicBool>) {
    let polled = Arc::new(AtomicBool::new(false));
    let recorder = Arc::clone(&polled);
    let stream = futures::stream::once(async move {
        recorder.store(true, Ordering::SeqCst);
        Ok::<_, std::io::Error>(Bytes::from(vec![b'x'; bytes]))
    });
    (Body::from_stream(stream), polled)
}

fn preflight_request(credential: Option<&str>, body: Body) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json");
    if let Some(credential) = credential {
        builder = builder.header(AUTHORIZATION, format!("Bearer {credential}"));
    }
    builder.body(body).expect("request")
}

/// A body the gateway accepts as far as parsing, and rejects there. It costs a
/// rate-limit slot and an inbound permit without contacting an upstream.
fn admitted_but_invalid() -> Body {
    Body::from("{}")
}

async fn error_code(response: Response<Body>) -> String {
    let body: Value = serde_json::from_slice(&response_body(response).await).expect("error JSON");
    body["error"]["code"]
        .as_str()
        .expect("error code")
        .to_string()
}

/// The ordering contract, stated so a reordering is observable. An anonymous
/// caller must not be able to make the gateway read - or buffer - a body it was
/// never entitled to send.
#[tokio::test]
async fn an_unauthenticated_oversized_body_is_refused_before_it_is_read() {
    let mut config = preflight_config();
    config.server.max_request_bytes = 16;
    let service = preflight_service(config);
    let (body, polled) = observed_body(1024);

    let response = service
        .handle_http_chat(preflight_request(None, body))
        .await;

    assert_eq!(response.status(), 401);
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );
    assert_eq!(error_code(response).await, "authentication_error");
    assert!(
        !polled.load(Ordering::SeqCst),
        "the body must not be read before the credential is checked"
    );
}

/// The other half of the ordering statement: the same body from an
/// authenticated caller is read, and then refused for its size. Without this
/// the test above would pass against a gateway that never enforced a body
/// bound at all.
#[tokio::test]
async fn an_authenticated_oversized_body_is_read_and_then_refused_as_too_large() {
    let mut config = preflight_config();
    config.server.max_request_bytes = 16;
    let service = preflight_service(config);
    let (body, polled) = observed_body(1024);

    let response = service
        .handle_http_chat(preflight_request(Some("inbound-test-key"), body))
        .await;

    assert_eq!(response.status(), 413);
    assert_eq!(error_code(response).await, "request_too_large");
    assert!(
        polled.load(Ordering::SeqCst),
        "an authenticated body is read up to the limit"
    );
}

/// Header bytes are bounded before authentication, because the authenticator
/// has to read the headers to do its work.
#[tokio::test]
async fn oversized_headers_are_refused_with_431() {
    let config = preflight_config();
    let limit = config.server.max_header_bytes;
    let service = preflight_service(config);
    let filler = "h".repeat(limit + 1);
    let (body, polled) = observed_body(1024);

    let response = service
        .handle_http_chat(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(AUTHORIZATION, "Bearer inbound-test-key")
                .header("x-octoroute-filler", filler)
                .body(body)
                .expect("request"),
        )
        .await;

    assert_eq!(response.status(), 431);
    assert_eq!(error_code(response).await, "headers_too_large");
    assert!(
        !polled.load(Ordering::SeqCst),
        "the body must not be read for a request refused on its headers"
    );

    // The control: the identical request without the filler header gets past
    // preflight and is refused on its content instead, so the 431 above is the
    // header bound and not some unrelated failure.
    let admitted = service
        .handle_http_chat(preflight_request(
            Some("inbound-test-key"),
            admitted_but_invalid(),
        ))
        .await;
    assert_eq!(admitted.status(), 400);
    assert_eq!(error_code(admitted).await, "invalid_request");
}

/// A throttled caller has to be told when to come back, or it retries
/// immediately and the limit buys nothing.
#[tokio::test]
async fn the_rate_limit_answers_429_with_a_usable_retry_after() {
    let mut config = preflight_config();
    config.server.requests_per_minute = 1;
    let service = preflight_service(config);

    let first = service
        .handle_http_chat(preflight_request(
            Some("inbound-test-key"),
            admitted_but_invalid(),
        ))
        .await;
    assert_eq!(first.status(), 400, "the first request spends the window");
    drop(first);

    let throttled = service
        .handle_http_chat(preflight_request(
            Some("inbound-test-key"),
            admitted_but_invalid(),
        ))
        .await;

    assert_eq!(throttled.status(), 429);
    let retry_after = throttled
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .expect("retry-after is required on a 429")
        .to_string();
    assert!(
        retry_after.parse::<u32>().is_ok_and(|seconds| seconds > 0),
        "retry-after must be a positive delay in seconds, got `{retry_after}`"
    );
    assert_eq!(error_code(throttled).await, "rate_limit_exceeded");
}

/// Inbound permits are held for the whole response lifetime, so exhaustion is
/// a distinct condition from the rate limit and reports its own code.
#[tokio::test]
async fn inbound_concurrency_exhaustion_is_reported_separately_from_the_rate_limit() {
    let mut config = preflight_config();
    config.server.max_in_flight = 1;
    let service = preflight_service(config);

    let held = service
        .handle_http_chat(preflight_request(
            Some("inbound-test-key"),
            admitted_but_invalid(),
        ))
        .await;
    assert_eq!(held.status(), 400);

    let refused = service
        .handle_http_chat(preflight_request(
            Some("inbound-test-key"),
            admitted_but_invalid(),
        ))
        .await;
    assert_eq!(refused.status(), 429);
    assert!(refused.headers().contains_key("retry-after"));
    assert_eq!(error_code(refused).await, "request_concurrency_limit");

    // The permit rides the response body, so releasing the first response is
    // what admits the next request.
    drop(held);
    let admitted = service
        .handle_http_chat(preflight_request(
            Some("inbound-test-key"),
            admitted_but_invalid(),
        ))
        .await;
    assert_eq!(admitted.status(), 400);
}

#[tokio::test]
async fn a_broken_body_returns_an_incomplete_request_error() {
    let service = preflight_service(preflight_config());
    for prefix in [None, Some(Bytes::from_static(b"{"))] {
        let chunks = prefix
            .into_iter()
            .map(Ok)
            .chain(std::iter::once(Err(std::io::Error::other(
                "body stream failed",
            ))));
        let response = service
            .handle_http_chat(preflight_request(
                Some("inbound-test-key"),
                Body::from_stream(futures::stream::iter(chunks)),
            ))
            .await;
        assert_eq!(response.status(), 400);
        assert_eq!(error_code(response).await, "request_body_incomplete");
    }
}

#[tokio::test]
async fn a_stalled_authenticated_body_times_out_and_releases_its_permit() {
    let mut config = preflight_config();
    config.server.max_in_flight = 1;
    config.server.request_body_timeout_ms = 20;
    let service = preflight_service(config);
    let stalled = Body::from_stream(futures::stream::pending::<Result<Bytes, std::io::Error>>());

    let timed_out = service
        .handle_http_chat(preflight_request(Some("inbound-test-key"), stalled))
        .await;

    assert_eq!(timed_out.status(), 408);
    assert_eq!(error_code(timed_out).await, "request_body_timeout");

    let admitted = service
        .handle_http_chat(preflight_request(
            Some("inbound-test-key"),
            admitted_but_invalid(),
        ))
        .await;
    assert_eq!(admitted.status(), 400);
    assert_eq!(error_code(admitted).await, "invalid_request");
}
