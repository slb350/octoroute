use super::{
    service_tests::{FakeResult, FakeTransport, authorized_headers, body, response_json, service},
    test_support::{gateway_config, gateway_config_with_server},
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use bytes::Bytes;
use wiremock::MockServer;

#[tokio::test]
async fn authentication_fails_before_parsing_routing_or_upstream_work() {
    let local = MockServer::start().await;
    let config = gateway_config(&local.uri(), "", "", "");
    let transport = FakeTransport::default();
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(&HeaderMap::new(), Bytes::from_static(b"not-json"))
        .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["www-authenticate"], "Bearer");
    assert_eq!(transport.local_calls(), 0);
    assert_eq!(transport.cloud_calls(), 0);
    assert!(
        local
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
    assert_eq!(
        response_json(response).await["error"]["code"],
        "authentication_error"
    );
}

#[tokio::test]
async fn oversized_headers_are_rejected_before_upstream_work() {
    let local = MockServer::start().await;
    let config = gateway_config_with_server(&local.uri(), "max_header_bytes = 64", "", "", "");
    let transport = FakeTransport::default();
    let gateway = service(config, transport.clone());
    let mut headers = authorized_headers();
    headers.insert(
        "x-oversized",
        HeaderValue::from_str(&"x".repeat(100)).expect("large test header"),
    );

    let response = gateway.handle_chat(&headers, body("cloud")).await;

    assert_eq!(
        response.status(),
        StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
    );
    assert_eq!(transport.local_calls(), 0);
    assert_eq!(transport.cloud_calls(), 0);
}

#[tokio::test]
async fn fixed_window_rate_limit_applies_after_authentication() {
    let local = MockServer::start().await;
    let config = gateway_config_with_server(&local.uri(), "requests_per_minute = 1", "", "", "");
    let transport = FakeTransport::default().with_cloud(FakeResult::Response(
        StatusCode::OK,
        r#"{"model":"openai/gpt-5.2","choices":[]}"#,
    ));
    let gateway = service(config, transport.clone());

    let first = gateway
        .handle_chat(&authorized_headers(), body("cloud"))
        .await;
    let second = gateway
        .handle_chat(&authorized_headers(), body("cloud"))
        .await;

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(second.headers()["retry-after"], "60");
    assert_eq!(transport.cloud_calls(), 1);
}
