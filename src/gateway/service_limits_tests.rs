use super::{
    service_tests::{FakeResult, FakeTransport, authorized_headers, body, response_json, service},
    test_support::{gateway_config, gateway_config_with_server},
};
use axum::http::StatusCode;
use wiremock::MockServer;

#[tokio::test]
async fn inbound_concurrency_permit_is_held_until_response_body_drops() {
    let local = MockServer::start().await;
    let config = gateway_config_with_server(&local.uri(), "max_in_flight = 1", "", "", "");
    let transport = FakeTransport::default()
        .with_cloud(FakeResult::Response(
            StatusCode::OK,
            r#"{"model":"openai/gpt-5.2","choices":[]}"#,
        ))
        .with_cloud(FakeResult::Response(
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
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    drop(first);

    let third = gateway
        .handle_chat(&authorized_headers(), body("cloud"))
        .await;
    assert_eq!(third.status(), StatusCode::OK);
    assert_eq!(transport.cloud_calls(), 2);
}

#[tokio::test]
async fn cloud_concurrency_permit_is_held_until_response_body_drops() {
    let local = MockServer::start().await;
    let config = gateway_config(&local.uri(), "", "max_in_flight = 1", "");
    let transport = FakeTransport::default()
        .with_cloud(FakeResult::Response(
            StatusCode::OK,
            r#"{"model":"openai/gpt-5.2","choices":[]}"#,
        ))
        .with_cloud(FakeResult::Response(
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
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        response_json(second).await["error"]["code"],
        "cloud_concurrency_limit"
    );
    drop(first);

    let third = gateway
        .handle_chat(&authorized_headers(), body("cloud"))
        .await;
    assert_eq!(third.status(), StatusCode::OK);
    assert_eq!(transport.cloud_calls(), 2);
}
