use super::{
    config::GatewayConfig,
    local::LocalLease,
    openrouter::OpenRouterRequest,
    service::GatewayService,
    test_support::{gateway_config, gateway_config_with_server},
    transport::{PreparedUpstreamResponse, UpstreamTransport},
};
use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Response, StatusCode, header::AUTHORIZATION},
};
use bytes::Bytes;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};

#[derive(Clone, Default)]
pub(super) struct FakeTransport {
    state: Arc<FakeState>,
}

#[derive(Default)]
struct FakeState {
    local_calls: AtomicUsize,
    cloud_calls: AtomicUsize,
    cloud_models: Mutex<Vec<String>>,
    local_results: Mutex<VecDeque<FakeResult>>,
    cloud_results: Mutex<VecDeque<FakeResult>>,
}

pub(super) enum FakeResult {
    Response(StatusCode, &'static str),
    ResponseWithRequestId(StatusCode, &'static str, &'static str),
    MidStreamError,
    Error,
}

#[derive(Debug)]
pub(super) struct FakeError;

impl fmt::Display for FakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("synthetic pre-commit failure")
    }
}

impl std::error::Error for FakeError {}

impl FakeTransport {
    pub(super) fn with_local(self, result: FakeResult) -> Self {
        self.state
            .local_results
            .lock()
            .expect("local queue")
            .push_back(result);
        self
    }

    pub(super) fn with_cloud(self, result: FakeResult) -> Self {
        self.state
            .cloud_results
            .lock()
            .expect("cloud queue")
            .push_back(result);
        self
    }

    pub(super) fn local_calls(&self) -> usize {
        self.state.local_calls.load(Ordering::SeqCst)
    }

    pub(super) fn cloud_calls(&self) -> usize {
        self.state.cloud_calls.load(Ordering::SeqCst)
    }

    pub(super) fn cloud_models(&self) -> Vec<String> {
        self.state
            .cloud_models
            .lock()
            .expect("cloud models")
            .clone()
    }

    fn next(queue: &Mutex<VecDeque<FakeResult>>) -> Result<PreparedUpstreamResponse, FakeError> {
        match queue.lock().expect("result queue").pop_front() {
            Some(FakeResult::Response(status, body)) => {
                let mut response = Response::new(Body::from(body));
                *response.status_mut() = status;
                Ok(PreparedUpstreamResponse::from_response(response))
            }
            Some(FakeResult::ResponseWithRequestId(status, body, request_id)) => {
                let mut response = Response::new(Body::from(body));
                *response.status_mut() = status;
                response
                    .headers_mut()
                    .insert("x-request-id", HeaderValue::from_static(request_id));
                Ok(PreparedUpstreamResponse::from_response(response))
            }
            Some(FakeResult::MidStreamError) => {
                let stream = futures::stream::iter([
                    Ok::<_, std::io::Error>(Bytes::from_static(b"data: first\n\n")),
                    Err(std::io::Error::other("synthetic mid-stream failure")),
                ]);
                Ok(PreparedUpstreamResponse::from_response(Response::new(
                    Body::from_stream(stream),
                )))
            }
            Some(FakeResult::Error) | None => Err(FakeError),
        }
    }
}

#[async_trait]
impl UpstreamTransport for FakeTransport {
    type Error = FakeError;

    async fn local(&self, _lease: LocalLease) -> Result<PreparedUpstreamResponse, Self::Error> {
        self.state.local_calls.fetch_add(1, Ordering::SeqCst);
        Self::next(&self.state.local_results)
    }

    async fn openrouter(
        &self,
        request: OpenRouterRequest,
    ) -> Result<PreparedUpstreamResponse, Self::Error> {
        self.state.cloud_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(model) = request.body()["model"].as_str() {
            self.state
                .cloud_models
                .lock()
                .expect("cloud models")
                .push(model.to_string());
        }
        Self::next(&self.state.cloud_results)
    }

    async fn openrouter_ready(&self) -> bool {
        true
    }
}

pub(super) fn authorized_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer inbound-secret"),
    );
    headers
}

pub(super) fn body(model: &str) -> Bytes {
    body_with_prompt(model, "hello")
}

pub(super) fn body_with_prompt(model: &str, prompt: &str) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}]
        }))
        .expect("serialize body"),
    )
}

pub(super) async fn response_json(response: Response<Body>) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 4096)
            .await
            .expect("response body"),
    )
    .expect("JSON response")
}

async fn mount_idle_local(server: &MockServer, slot_calls: u64) {
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
        .expect(slot_calls)
        .mount(server)
        .await;
}

pub(super) async fn mount_local_admission(server: &MockServer) {
    mount_idle_local(server, 1).await;
    mount_input_tokens(server, 10).await;
}

async fn mount_busy_local_admission(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .and(query_param("fail_on_no_slot", "1"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(server)
        .await;
}

pub(super) async fn mount_intelligent_route(
    server: &MockServer,
    destination: &str,
    slot_calls: u64,
) {
    let content =
        serde_json::to_string(&json!({"destination": destination})).expect("route decision");
    mount_intelligent_response(server, &content, slot_calls).await;
}

pub(super) async fn mount_intelligent_response(
    server: &MockServer,
    content: &str,
    slot_calls: u64,
) {
    mount_idle_local(server, slot_calls).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": content}}]
        })))
        .expect(1)
        .mount(server)
        .await;
}

pub(super) async fn mount_auto_local_admission(server: &MockServer) {
    mount_intelligent_route(server, "local", 1).await;
    mount_input_tokens(server, 10).await;
}

pub(super) async fn mount_input_tokens(server: &MockServer, input_tokens: u32) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"input_tokens": input_tokens})),
        )
        .expect(1)
        .mount(server)
        .await;
}

pub(super) fn service(
    config: GatewayConfig,
    transport: FakeTransport,
) -> GatewayService<FakeTransport> {
    GatewayService::new(config, transport).expect("gateway service")
}

#[tokio::test]
async fn auto_spills_to_cloud_and_records_when_local_is_busy() {
    let local = MockServer::start().await;
    mount_busy_local_admission(&local).await;
    let config = gateway_config(&local.uri(), "", "", "");
    let transport = FakeTransport::default().with_cloud(FakeResult::Response(
        StatusCode::OK,
        r#"{"model":"google/gemini-2.5-flash-lite","choices":[]}"#,
    ));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(&authorized_headers(), body("auto"))
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "cloud");
    assert_eq!(response.headers()["x-octoroute-reason"], "local_busy");
    assert_eq!(transport.local_calls(), 0);
    assert_eq!(transport.cloud_calls(), 1);
    assert!(
        gateway
            .metrics_text()
            .expect("metrics")
            .contains("octoroute_local_busy_spillovers_total 1")
    );
}

#[tokio::test]
async fn mid_stream_failure_is_forwarded_once_and_recorded_for_actual_upstream() {
    let local = MockServer::start().await;
    let config = gateway_config(&local.uri(), "", "", "");
    let transport = FakeTransport::default().with_cloud(FakeResult::MidStreamError);
    let gateway = service(config, transport);

    let response = gateway
        .handle_chat(&authorized_headers(), body("cloud"))
        .await;
    let stream_error = to_bytes(response.into_body(), 4096)
        .await
        .expect_err("synthetic stream must fail");

    assert!(
        stream_error
            .to_string()
            .contains("synthetic mid-stream failure")
    );
    let metrics = gateway.metrics_text().expect("metrics");
    assert!(metrics.contains(
        "octoroute_upstream_failures_total{phase=\"mid_stream\",upstream=\"openrouter\"} 1"
    ));
}

#[tokio::test]
async fn explicit_cloud_never_probes_local_and_preserves_selected_model() {
    let local = MockServer::start().await;
    let config = gateway_config(&local.uri(), "", "", "");
    let transport = FakeTransport::default().with_cloud(FakeResult::Response(
        StatusCode::OK,
        r#"{"model":"deepseek/deepseek-v4-flash","choices":[]}"#,
    ));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(&authorized_headers(), body("deepseek/deepseek-v4-flash"))
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "cloud");
    assert_eq!(transport.local_calls(), 0);
    assert_eq!(transport.cloud_calls(), 1);
    assert!(
        local
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );
    assert_eq!(
        response_json(response).await["model"],
        "deepseek/deepseek-v4-flash"
    );
    let metrics = gateway.metrics_text().expect("metrics");
    assert!(metrics.contains(
        "octoroute_upstream_requests_total{outcome=\"response\",status_class=\"2xx\",upstream=\"openrouter\"} 1"
    ));
    assert!(
        metrics.contains("octoroute_time_to_first_byte_seconds_count{destination=\"cloud\"} 1")
    );
    assert!(metrics.contains("octoroute_routing_duration_seconds_count 1"));
    assert!(metrics.contains("octoroute_request_duration_seconds_count{destination=\"cloud\"} 1"));
    assert!(metrics.contains("octoroute_in_flight_requests{destination=\"cloud\"} 0"));
}

#[tokio::test]
async fn committed_response_preserves_safe_upstream_request_id() {
    let local = MockServer::start().await;
    let config = gateway_config(&local.uri(), "", "", "");
    let transport = FakeTransport::default().with_cloud(FakeResult::ResponseWithRequestId(
        StatusCode::OK,
        r#"{"model":"openai/gpt-5.2","choices":[]}"#,
        "upstream-request-123",
    ));
    let gateway = service(config, transport);

    let response = gateway
        .handle_chat(&authorized_headers(), body("cloud"))
        .await;

    assert_eq!(response.headers()["x-request-id"], "upstream-request-123");
}

#[tokio::test]
async fn auto_falls_back_to_cloud_on_retryable_local_status_before_commit() {
    let local = MockServer::start().await;
    mount_auto_local_admission(&local).await;
    let config = gateway_config(&local.uri(), "", "", "");
    let transport = FakeTransport::default()
        .with_local(FakeResult::Response(
            StatusCode::SERVICE_UNAVAILABLE,
            r#"{"error":{"message":"busy"}}"#,
        ))
        .with_cloud(FakeResult::Response(
            StatusCode::OK,
            r#"{"model":"google/gemini-3.5-flash","choices":[]}"#,
        ));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(&authorized_headers(), body("auto"))
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "cloud");
    assert_eq!(
        response.headers()["x-octoroute-reason"],
        "local_early_failure"
    );
    assert_eq!(transport.local_calls(), 1);
    assert_eq!(transport.cloud_calls(), 1);
    assert!(
        gateway
            .metrics_text()
            .expect("metrics")
            .contains("octoroute_local_fallbacks_total 1")
    );
    assert_eq!(
        response_json(response).await["model"],
        "google/gemini-3.5-flash"
    );
}

#[tokio::test]
async fn explicit_local_never_falls_back_after_retryable_local_status() {
    let local = MockServer::start().await;
    mount_local_admission(&local).await;
    let config = gateway_config(&local.uri(), "", "", "");
    let transport = FakeTransport::default().with_local(FakeResult::Response(
        StatusCode::SERVICE_UNAVAILABLE,
        r#"{"error":{"message":"busy"}}"#,
    ));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(&authorized_headers(), body("local"))
        .await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["x-octoroute-destination"], "local");
    assert_eq!(transport.local_calls(), 1);
    assert_eq!(transport.cloud_calls(), 0);
}

#[tokio::test]
async fn automatic_local_connect_failure_falls_back_but_explicit_local_does_not() {
    for (model, expected_cloud) in [("auto", 1), ("local", 0)] {
        let local = MockServer::start().await;
        if model == "auto" {
            mount_auto_local_admission(&local).await;
        } else {
            mount_local_admission(&local).await;
        }
        let config = gateway_config(&local.uri(), "", "", "");
        let mut transport = FakeTransport::default().with_local(FakeResult::Error);
        if expected_cloud == 1 {
            transport = transport.with_cloud(FakeResult::Response(
                StatusCode::OK,
                r#"{"model":"openai/gpt-5.2","choices":[]}"#,
            ));
        }
        let gateway = service(config, transport.clone());

        let response = gateway
            .handle_chat(&authorized_headers(), body(model))
            .await;

        assert_eq!(transport.cloud_calls(), expected_cloud);
        assert_eq!(
            response.status(),
            if expected_cloud == 1 {
                StatusCode::OK
            } else {
                StatusCode::BAD_GATEWAY
            }
        );
    }
}

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
