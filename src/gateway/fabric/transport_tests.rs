//! Transport header-allowlist and deadline tests.

use super::transport::*;
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use std::time::Duration;

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
        ("x-request-id", "req-1"),
        ("x-generation-id", "gen-1"),
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
    assert_eq!(safe.get("x-request-id").expect("x-request-id"), "req-1");
    assert_eq!(
        safe.get("x-generation-id").expect("x-generation-id"),
        "gen-1"
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
