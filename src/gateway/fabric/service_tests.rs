use super::{FabricConfig, FabricGatewayService};
use crate::gateway::config::Environment;
use axum::{
    body::{Bytes, to_bytes},
    http::{HeaderMap, HeaderValue, header::AUTHORIZATION},
};
use reqwest::Url;
use serde_json::json;
use std::collections::BTreeMap;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path, query_param},
};

#[derive(Debug, Default)]
struct TestEnvironment(BTreeMap<String, String>);

impl TestEnvironment {
    fn with(mut self, name: &str, value: &str) -> Self {
        self.0.insert(name.to_string(), value.to_string());
        self
    }
}

impl Environment for TestEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }
}

fn headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("Bearer inbound-test-key"),
    );
    headers
}

fn local_config(server: &MockServer) -> FabricConfig {
    let mut config =
        FabricConfig::from_toml(include_str!("../../../config.v3.toml")).expect("config");
    let workers = config.local_pools.get_mut("workers").expect("workers pool");
    workers.members.truncate(1);
    workers.members[0].base_url = Url::parse(&server.uri()).expect("mock URL");
    config
}

async fn mount_ready_local(server: &MockServer) {
    let request_body = json!({
        "model": "coding-worker-model",
        "messages": [{"role": "user", "content": "implement the bounded task"}],
        "stream": true,
        "max_completion_tokens": 1024,
        "reasoning_effort": "medium"
    });
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
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .and(body_json(request_body.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 128})))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_json(request_body))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(
                    "data: {\"id\":\"local\"}\n\ndata: [DONE]\n\n",
                    "text/event-stream",
                ),
        )
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn v3_worker_route_streams_through_shared_precommit_transport() {
    let server = MockServer::start().await;
    mount_ready_local(&server).await;
    let service = FabricGatewayService::from_config(
        local_config(&server),
        &TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "worker",
            "messages": [{"role": "user", "content": "implement the bounded task"}],
            "stream": true,
            "max_completion_tokens": 1024,
            "reasoning_effort": "medium"
        }))
        .expect("JSON"),
    );

    let response = service.handle_chat(&headers(), body).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-destination")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-pool")
            .and_then(|value| value.to_str().ok()),
        Some("workers")
    );
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-member")
            .and_then(|value| value.to_str().ok()),
        Some("worker-0")
    );
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("stream body");
    assert!(
        std::str::from_utf8(&body)
            .expect("UTF-8")
            .contains("[DONE]")
    );
}

#[tokio::test]
async fn configured_provider_route_fails_closed_until_adapter_is_enabled() {
    let server = MockServer::start().await;
    let service = FabricGatewayService::from_config(
        local_config(&server),
        &TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "review the architecture"}]
        }))
        .expect("JSON"),
    );

    let response = service.handle_chat(&headers(), body).await;
    assert_eq!(response.status(), 503);
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("error body");
    assert!(
        std::str::from_utf8(&body)
            .expect("UTF-8")
            .contains("provider_runtime_unavailable")
    );
}

#[tokio::test]
async fn v3_models_include_auto_and_all_virtual_routes() {
    let server = MockServer::start().await;
    let service = FabricGatewayService::from_config(
        local_config(&server),
        &TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");
    let models = service.model_ids();
    assert!(models.contains(&"auto".to_string()));
    for model in ["auto-route", "worker", "supervisor", "local", "cloud-sota"] {
        assert!(models.contains(&model.to_string()), "missing {model}");
    }
}
