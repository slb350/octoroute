//! Transport header-allowlist and deadline tests.

use super::transport::*;
use crate::gateway::http_client::build as build_http_client;
use axum::{
    body::to_bytes,
    http::{HeaderMap, HeaderName, HeaderValue},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

/// The header allowlist is the only thing preventing an upstream
/// `set-cookie` or `www-authenticate` from reaching the client. Replacing
/// the function body with `source.clone()` must fail this test.
#[test]
fn upstream_credential_and_session_headers_never_reach_the_client() {
    let mut source = HeaderMap::new();
    for (name, value) in [
        ("set-cookie", "session=secret; HttpOnly"),
        ("www-authenticate", "Bearer realm=\"upstream\""),
        ("authorization", "Bearer upstream-key"),
        ("proxy-authenticate", "Basic realm=\"proxy\""),
        ("x-api-key", "upstream-key"),
        ("server", "upstream/1.2.3"),
        ("strict-transport-security", "max-age=31536000"),
        ("access-control-allow-origin", "*"),
    ] {
        source.append(
            HeaderName::from_static(name),
            HeaderValue::from_str(value).expect("header value"),
        );
    }

    let safe = safe_response_headers(&source);
    for name in [
        "set-cookie",
        "www-authenticate",
        "authorization",
        "proxy-authenticate",
        "x-api-key",
        "server",
        "strict-transport-security",
        "access-control-allow-origin",
    ] {
        assert!(
            !safe.contains_key(name),
            "`{name}` must not be forwarded to the client"
        );
    }
    assert!(safe.is_empty(), "only allowlisted headers may survive");
}

/// Safe diagnostics are preserved: request IDs and rate-limit fields are
/// what a client needs to correlate and back off.
#[test]
fn safe_diagnostic_headers_are_preserved() {
    let mut source = HeaderMap::new();
    for (name, value) in [
        ("content-type", "text/event-stream"),
        ("retry-after", "30"),
        ("x-generation-id", "gen-1"),
        ("openai-request-id", "openai-1"),
        ("x-ratelimit-remaining-tokens", "1000"),
    ] {
        source.append(
            HeaderName::from_static(name),
            HeaderValue::from_str(value).expect("header value"),
        );
    }

    let safe = safe_response_headers(&source);
    assert_eq!(
        safe.get("content-type").expect("content-type"),
        "text/event-stream"
    );
    assert_eq!(safe.get("retry-after").expect("retry-after"), "30");
    assert_eq!(
        safe.get("x-generation-id").expect("x-generation-id"),
        "gen-1"
    );
    assert_eq!(
        safe.get("openai-request-id").expect("openai-request-id"),
        "openai-1"
    );
    assert_eq!(
        safe.get("x-ratelimit-remaining-tokens")
            .expect("rate limit header"),
        "1000"
    );
}

/// A missing first-byte deadline must not invent one, and a configured one
/// must bound how long a hung upstream holds its permits.
#[tokio::test]
async fn first_byte_deadline_is_applied_only_when_configured() {
    let unbounded = UpstreamDeadlines::new(60_000, None);
    let slow = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        Ok::<_, FabricTransportError>(())
    };
    unbounded
        .hold_first_byte(slow)
        .await
        .expect("no deadline was configured");

    let bounded = UpstreamDeadlines::new(60_000, Some(10));
    let hung = async {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok::<_, FabricTransportError>(())
    };
    assert!(matches!(
        bounded.hold_first_byte(hung).await,
        Err(FabricTransportError::FirstByteTimeout)
    ));
}

/// The gateway sets `x-request-id` only when the header is absent, so an
/// upstream that sends its own would take over the gateway's correlation id.
/// Upstream identifiers still reach the client under their own names.
#[test]
fn an_upstream_cannot_claim_the_gateway_request_id() {
    let mut source = HeaderMap::new();
    source.append(
        HeaderName::from_static("x-request-id"),
        HeaderValue::from_static("upstream-request-id"),
    );
    source.append(
        HeaderName::from_static("openai-request-id"),
        HeaderValue::from_static("upstream-request-id"),
    );

    let safe = safe_response_headers(&source);
    assert!(!safe.contains_key("x-request-id"));
    assert_eq!(
        safe.get("openai-request-id").expect("openai-request-id"),
        "upstream-request-id"
    );
}

/// The shared client refuses a redirect instead of following it.
///
/// reqwest does not strip a custom `x-api-key` across origins, so a followed
/// 3xx would hand the provider credential to whatever host the redirect names,
/// and would rewrite the POST to a GET against an endpoint no operator
/// configured. Relaxing `Policy::none()` must fail this test.
#[tokio::test]
async fn the_shared_client_never_follows_an_upstream_redirect() {
    let target = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string("hijacked"))
        .expect(0)
        .mount(&target)
        .await;
    let upstream = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(307).insert_header("location", target.uri().as_str()))
        .expect(1)
        .mount(&upstream)
        .await;

    let client = build_http_client().expect("shared client");
    let response = client
        .post(upstream.uri())
        .header("x-api-key", "provider-credential")
        .body("{}")
        .send()
        .await
        .expect("upstream response");

    assert_eq!(response.status(), 307);
    assert!(
        target
            .received_requests()
            .await
            .expect("request recording")
            .is_empty(),
        "the redirect target must never receive the provider credential"
    );
}

async fn translated_anthropic_error_code(status: u16) -> String {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(status).set_body_json(json!({
            "type": "error",
            "error": {"type": "fixture_error", "message": "bounded fixture"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = build_http_client().expect("shared client");
    let response = client.get(server.uri()).send().await.expect("response");
    let permit = Arc::new(Semaphore::new(1))
        .acquire_owned()
        .await
        .expect("permit");
    let response = prepare_anthropic_for_test(response, "k3", false, permit)
        .await
        .expect("translated error")
        .into_response();
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("error body");
    let body: Value = serde_json::from_slice(&body).expect("error JSON");
    body["error"]["code"]
        .as_str()
        .expect("bounded error code")
        .to_string()
}

/// Credential rejections and ordinary request errors are observably distinct
/// before commitment. Forcing the predicate either true or false must break
/// one side of this table.
///
/// Every arm of the classification is listed, not only the credential pair: an
/// arm that no row distinguishes can be deleted and still satisfy the rows that
/// remain, because each one falls through to the catch-all.
#[tokio::test]
async fn anthropic_errors_distinguish_credential_rejections_from_request_failures() {
    for status in [401, 403] {
        assert_eq!(
            translated_anthropic_error_code(status).await,
            "provider_authentication_failed",
            "{status} refuses the provider credential"
        );
    }
    assert_eq!(
        translated_anthropic_error_code(429).await,
        "provider_rate_limited",
        "429 is capacity, not a malformed request"
    );
    for status in [500, 503] {
        assert_eq!(
            translated_anthropic_error_code(status).await,
            "provider_server_error",
            "{status} is the provider's own failure"
        );
    }
    assert_eq!(
        translated_anthropic_error_code(400).await,
        "provider_request_failed"
    );
}

/// Serve one response with a `content-length` the body never satisfies, then
/// close the connection. `body_bytes` is how much of that body arrives first.
///
/// Wiremock cannot express a truncated body, and a truncated body is the whole
/// point: it is the difference between an upstream that never answered and one
/// that answered and then failed part-way through.
async fn truncated_upstream(body_bytes: usize) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("connection");
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100000\r\n\r\n",
            )
            .await
            .expect("response head");
        socket
            .write_all(&vec![b'x'; body_bytes])
            .await
            .expect("partial body");
        socket.flush().await.expect("flush");
    });
    format!("http://{address}/")
}

/// An upstream that never sent a byte and one that failed part-way through its
/// body are different faults, and the transport has to name them apart: the
/// first is an upstream that did not answer, the second is one that answered
/// and then broke. Collapsing them tells an operator to look at the wrong
/// thing.
#[tokio::test]
async fn a_body_that_fails_before_and_after_its_first_byte_are_distinct_faults() {
    let client = build_http_client().expect("shared client");

    let silent = truncated_upstream(0).await;
    let response = client.get(&silent).send().await.expect("response head");
    assert!(
        matches!(
            read_bounded(response, MAX_TRANSLATED_RESPONSE_BYTES).await,
            Err(FabricTransportError::ReadFirstChunk(_))
        ),
        "a body that never started is a first-chunk failure"
    );

    let truncated = truncated_upstream(4096).await;
    let response = client.get(&truncated).send().await.expect("response head");
    assert!(
        matches!(
            read_bounded(response, MAX_TRANSLATED_RESPONSE_BYTES).await,
            Err(FabricTransportError::ReadBody(_))
        ),
        "a body that broke after its first bytes is a mid-body failure"
    );
}

/// The buffered-translation bound is 16 MiB, and a response has to be measured
/// against that rather than against some fraction of it.
///
/// A megabyte-scale Anthropic answer is ordinary - a long structured reply with
/// tool arguments reaches it - so a bound accidentally reduced to kilobytes
/// would refuse real traffic while every existing test, whose fixtures are a
/// few hundred bytes, kept passing.
#[tokio::test]
async fn a_multi_megabyte_response_is_within_the_translation_bound() {
    let body = "x".repeat(2 * 1024 * 1024);
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string(body.clone()))
        .mount(&server)
        .await;
    let client = build_http_client().expect("shared client");

    let response = client.get(server.uri()).send().await.expect("response");
    let read = read_bounded(response, MAX_TRANSLATED_RESPONSE_BYTES)
        .await
        .expect("a 2 MiB response is within the 16 MiB bound");
    assert_eq!(read.len(), body.len());

    // The control: the same response against a bound it does not fit is
    // refused, so the read above is the bound admitting it rather than the
    // limit being ignored.
    let response = client.get(server.uri()).send().await.expect("response");
    assert!(matches!(
        read_bounded(response, 64 * 1024).await,
        Err(FabricTransportError::ProviderResponseTooLarge)
    ));
}
