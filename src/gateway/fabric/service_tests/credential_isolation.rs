//! Credential isolation: the inbound bearer never reaches any upstream.

use super::*;

/// Local members in this configuration carry no credential, so the member must
/// see no `Authorization` at all. A regression that forwarded the inbound
/// bearer would put the gateway's own client credential on the wire.
#[tokio::test]
async fn the_inbound_bearer_never_reaches_a_local_member() {
    let server = MockServer::start().await;
    mount_local_admission(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"id": "local", "choices": []})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let service = FabricGatewayService::from_config(
        local_config(&server),
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    let response = service
        .handle_chat(&headers(), local_request("local"))
        .await;

    assert_eq!(response.status(), 200);
    for received in server.received_requests().await.expect("request recording") {
        assert!(
            received.headers.get("authorization").is_none(),
            "`{}` must carry no Authorization header",
            received.url
        );
    }
}

/// The upstream's own credential must be the only `Authorization` it receives.
/// The existing `header(...)` matchers prove the right value is present; a
/// regression that forwarded the inbound bearer as well would still satisfy
/// them, so this asserts the complete set of values.
#[tokio::test]
async fn an_open_ai_provider_receives_only_its_own_credential() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"id": "cloud", "choices": []})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let service = FabricGatewayService::from_config(
        single_provider_config(&server, "zai"),
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("ZAI_API_KEY", "zai-test-key"),
    )
    .expect("service");

    let response = service.handle_chat(&headers(), cloud_request()).await;

    assert_eq!(response.status(), 200);
    let received = server.received_requests().await.expect("request recording");
    let upstream = received.last().expect("one upstream request");
    let authorization: Vec<&str> = upstream
        .headers
        .get_all("authorization")
        .iter()
        .map(|value| value.to_str().expect("ASCII header value"))
        .collect();
    assert_eq!(authorization, vec!["Bearer zai-test-key"]);
}

/// The Anthropic protocol carries the credential in `x-api-key`, so a forwarded
/// inbound bearer would ride along in an `Authorization` header nothing else
/// inspects.
#[tokio::test]
async fn an_anthropic_provider_receives_no_inbound_authorization() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "type": "message",
                    "id": "msg-1",
                    "model": "k3",
                    "content": [{"type": "text", "text": "ok"}]
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let service = FabricGatewayService::from_config(
        single_provider_config(&server, "kimi"),
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("KIMI_API_KEY", "kimi-test-key"),
    )
    .expect("service");

    let response = service
        .handle_chat(&headers(), portable_cloud_request())
        .await;

    assert_eq!(response.status(), 200);
    let received = server.received_requests().await.expect("request recording");
    let upstream = received.last().expect("one upstream request");
    assert_eq!(
        upstream
            .headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        Some("kimi-test-key")
    );
    assert!(
        upstream.headers.get("authorization").is_none(),
        "the inbound bearer must never travel to an Anthropic provider"
    );
}
