use super::{
    local::{AdmissionOutcome, LlamaCppAdmission},
    openrouter::OpenRouterRequest,
    request::GatewayRequest,
    routing::ModelIntent,
    test_support::{gateway_config, gateway_request},
    transport::{GatewayTransport, GatewayTransportError, UpstreamTransport},
};
use axum::body::to_bytes;
use serde_json::json;
use std::time::Duration;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

async fn local_lease(
    server: &MockServer,
    config: &super::config::GatewayConfig,
    request: &GatewayRequest,
) -> (LlamaCppAdmission, super::local::LocalLease) {
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .and(query_param("fail_on_no_slot", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .expect(2)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"object": "response.input_tokens", "input_tokens": 10})),
        )
        .expect(2)
        .mount(server)
        .await;

    let admission = LlamaCppAdmission::new(config.local()).expect("admission controller");
    match admission.try_admit(request).await.expect("admission") {
        AdmissionOutcome::Admitted(lease) => (admission, lease),
        AdmissionOutcome::Rejected(state) => panic!("unexpected rejection: {state:?}"),
    }
}

#[test]
fn openrouter_request_uses_cloud_credential_and_correct_nested_base_path() {
    let config = gateway_config(
        "http://127.0.0.1:8080",
        "",
        r#"app_title = "Personal Octoroute""#,
        "",
    );
    let gateway_request = gateway_request(json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [{"role": "user", "content": "hello"}]
    }));
    let openrouter = OpenRouterRequest::build(
        gateway_request,
        &ModelIntent::CloudModel("deepseek/deepseek-v4-flash".to_string()),
        config.openrouter(),
    )
    .expect("OpenRouter body");
    let transport = GatewayTransport::new(&config).expect("transport");

    let request = transport
        .openrouter_request(&openrouter)
        .expect("HTTP request");

    assert_eq!(
        request.url().as_str(),
        "https://openrouter.ai/api/v1/chat/completions"
    );
    assert_eq!(
        request.headers()["authorization"],
        "Bearer openrouter-secret"
    );
    assert_eq!(
        request.headers()["x-openrouter-title"],
        "Personal Octoroute"
    );
    assert!(request.headers().get("cookie").is_none());
    assert!(request.headers().get("proxy-authorization").is_none());

    let health = transport
        .openrouter_health_request()
        .expect("health request");
    assert_eq!(health.url().as_str(), "https://openrouter.ai/api/v1/key");
    assert_eq!(
        health.headers()["authorization"],
        "Bearer openrouter-secret"
    );
}

#[tokio::test]
async fn configured_local_first_byte_timeout_cancels_and_releases_capacity() {
    let server = MockServer::start().await;
    let config = gateway_config(&server.uri(), "first_byte_timeout_ms = 10", "", "");
    let gateway_request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}]
    }));
    let (admission, lease) = local_lease(&server, &config, &gateway_request).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(json!({"choices": []})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let transport = GatewayTransport::new(&config).expect("transport");

    let error = match transport.local(lease).await {
        Ok(_) => panic!("first response byte must time out"),
        Err(error) => error,
    };

    assert!(matches!(error, GatewayTransportError::FirstByteTimeout));
    assert_eq!(
        admission
            .try_admit(&gateway_request)
            .await
            .expect("released lease state")
            .state(),
        super::routing::LocalAdmissionState::Ready
    );
}

#[tokio::test]
async fn dropping_local_response_body_releases_capacity_for_client_cancellation() {
    let server = MockServer::start().await;
    let config = gateway_config(&server.uri(), "", "", "");
    let gateway_request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "stream": true
    }));
    let (admission, lease) = local_lease(&server, &config, &gateway_request).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_bytes(b"data: first\n\ndata: second\n\n"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let transport = GatewayTransport::new(&config).expect("transport");

    let response = transport
        .local(lease)
        .await
        .expect("prepared local response");
    assert_eq!(
        admission
            .try_admit(&gateway_request)
            .await
            .expect("held lease")
            .state(),
        super::routing::LocalAdmissionState::Busy
    );

    drop(response);

    assert_eq!(
        admission
            .try_admit(&gateway_request)
            .await
            .expect("released lease")
            .state(),
        super::routing::LocalAdmissionState::Ready
    );
}

#[tokio::test]
async fn local_response_stream_preserves_body_and_only_safe_headers() {
    let server = MockServer::start().await;
    let config = gateway_config(&server.uri(), r#"api_key_env = "LOCAL_API_KEY""#, "", "");
    let gateway_request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "unknown_field": {"preserved": true}
    }));
    let (admission, lease) = local_lease(&server, &config, &gateway_request).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer local-secret"))
        .and(body_json(json!({
            "model": "puzzle-75b",
            "messages": [{"role": "user", "content": "hello"}],
            "unknown_field": {"preserved": true}
        })))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("content-type", "text/event-stream")
                .insert_header("cache-control", "no-cache")
                .insert_header("x-request-id", "upstream-123")
                .insert_header("x-generation-id", "generation-456")
                .insert_header("connection", "keep-alive")
                .insert_header("set-cookie", "private=value")
                .set_body_bytes(b"data: first\n\ndata: second\n\n"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let transport = GatewayTransport::new(&config).expect("transport");

    let pending = transport
        .send_local(lease)
        .await
        .expect("local response headers");
    assert_eq!(pending.status().as_u16(), 201);
    let response = pending.prepare().await.expect("first response bytes");

    assert_eq!(response.status().as_u16(), 201);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert_eq!(response.headers()["cache-control"], "no-cache");
    assert_eq!(response.headers()["x-request-id"], "upstream-123");
    assert_eq!(response.headers()["x-generation-id"], "generation-456");
    assert!(response.headers().get("connection").is_none());
    assert!(response.headers().get("set-cookie").is_none());
    assert_eq!(
        admission
            .try_admit(&gateway_request)
            .await
            .expect("held lease state")
            .state(),
        super::routing::LocalAdmissionState::Busy
    );
    let body = to_bytes(response.into_body(), 1024)
        .await
        .expect("streamed body");
    assert_eq!(body, "data: first\n\ndata: second\n\n");
    assert_eq!(
        admission
            .try_admit(&gateway_request)
            .await
            .expect("released lease state")
            .state(),
        super::routing::LocalAdmissionState::Ready
    );
}
