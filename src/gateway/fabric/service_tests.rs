use super::{FabricConfig, FabricGatewayService, RouteTarget};
use crate::gateway::env::Environment;
use axum::{
    body::{Bytes, to_bytes},
    http::{HeaderMap, HeaderValue, header::AUTHORIZATION},
};
use reqwest::Url;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

#[derive(Debug, Clone, Default)]
struct TestEnvironment {
    values: BTreeMap<String, String>,
    reads: Arc<Mutex<Vec<String>>>,
}

impl TestEnvironment {
    fn with(mut self, name: &str, value: &str) -> Self {
        self.values.insert(name.to_string(), value.to_string());
        self
    }

    fn reads(&self) -> Vec<String> {
        self.reads.lock().expect("reads mutex").clone()
    }
}

impl Environment for TestEnvironment {
    fn get(&self, name: &str) -> Option<String> {
        self.reads
            .lock()
            .expect("reads mutex")
            .push(name.to_string());
        self.values.get(name).cloned()
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
    let mut config = FabricConfig::from_toml(include_str!("../../../config.toml")).expect("config");
    let workers = config.local_pools.get_mut("workers").expect("workers pool");
    workers.members.truncate(1);
    workers.members[0].base_url = Url::parse(&server.uri()).expect("mock URL");
    config
}

fn single_provider_config(server: &MockServer, provider: &str) -> FabricConfig {
    let mut config = local_config(server);
    let endpoint = Url::parse(&format!("{}/", server.uri())).expect("mock provider URL");
    config
        .providers
        .get_mut(provider)
        .expect("configured provider")
        .endpoint = Some(endpoint);
    config
        .routes
        .get_mut("cloud-sota")
        .expect("cloud route")
        .steps = vec![RouteTarget::Provider(provider.to_string())];
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
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("KIMI_API_KEY", "unused-kimi-key")
        .with("ZAI_API_KEY", "unused-zai-key")
        .with("OPENROUTER_API_KEY", "unused-openrouter-key");
    let environment_audit = environment.clone();
    let service =
        FabricGatewayService::from_config(local_config(&server), environment).expect("service");
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
    let reads = environment_audit.reads();
    assert_eq!(reads, vec!["OCTOROUTE_API_KEY"]);
}

#[tokio::test]
async fn local_only_failure_never_resolves_or_contacts_a_provider() {
    let server = MockServer::start().await;
    let mut config = local_config(&server);
    for pool in config.local_pools.values_mut() {
        pool.enabled = false;
    }
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("KIMI_API_KEY", "unused-kimi-key")
        .with("ZAI_API_KEY", "unused-zai-key")
        .with("OPENROUTER_API_KEY", "unused-openrouter-key");
    let environment_audit = environment.clone();
    let service = FabricGatewayService::from_config(config, environment).expect("service");
    let mut request_headers = headers();
    request_headers.insert(
        "x-octoroute-privacy",
        HeaderValue::from_static("local-only"),
    );
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "keep this local"}]
        }))
        .expect("JSON"),
    );

    let response = service.handle_chat(&request_headers, body).await;
    assert_eq!(response.status(), 503);
    assert_eq!(environment_audit.reads(), vec!["OCTOROUTE_API_KEY"]);
}

#[tokio::test]
async fn unsupported_provider_kind_fails_closed_without_resolving_cloud_credentials() {
    let server = MockServer::start().await;
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("OPENROUTER_API_KEY", "unused-openrouter-key");
    let environment_audit = environment.clone();
    let service =
        FabricGatewayService::from_config(local_config(&server), environment).expect("service");
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
            .contains("provider_incompatible")
    );
    assert_eq!(environment_audit.reads(), vec!["OCTOROUTE_API_KEY"]);
}

#[tokio::test]
async fn open_ai_provider_rewrites_only_destination_and_supplies_bounded_headers() {
    let server = MockServer::start().await;
    let expected_request = json!({
        "model": "glm-5.3",
        "messages": [{"role": "user", "content": "review the architecture"}],
        "stream": true,
        "reasoning_effort": "high",
        "temperature": 0.7,
        "future_field": {"preserved": true}
    });
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer zai-test-key"))
        .and(body_json(expected_request))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-request-id", "provider-request-id")
                .set_body_raw(
                    "data: {\"id\":\"cloud\",\"model\":\"glm-5.3\"}\n\ndata: [DONE]\n\n",
                    "text/event-stream",
                ),
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
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "review the architecture"}],
            "stream": true,
            "reasoning_effort": "high",
            "temperature": 0.7,
            "future_field": {"preserved": true}
        }))
        .expect("JSON"),
    );

    let response = service.handle_chat(&headers(), body).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-provider")
            .and_then(|value| value.to_str().ok()),
        Some("zai")
    );
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-destination")
            .and_then(|value| value.to_str().ok()),
        Some("cloud")
    );
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some("provider-request-id")
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
async fn missing_provider_credential_falls_forward_before_prompt_disclosure() {
    let server = MockServer::start().await;
    let expected_request = json!({
        "model": "openrouter/auto",
        "messages": [{"role": "user", "content": "review the architecture"}],
        "reasoning_effort": "xhigh",
        "temperature": 0.2,
        "plugins": [
            {"id": "preserved-plugin", "setting": true},
            {"id": "auto-router", "cost_quality_tradeoff": 9}
        ]
    });
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer openrouter-test-key"))
        .and(header("x-openrouter-title", "Octoroute"))
        .and(body_json(expected_request))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"id": "cloud", "model": "openrouter/auto", "choices": []})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut config = local_config(&server);
    let endpoint = Url::parse(&format!("{}/", server.uri())).expect("mock provider URL");
    config.providers.get_mut("zai").expect("zai").endpoint = Some(endpoint.clone());
    config
        .providers
        .get_mut("openrouter")
        .expect("openrouter")
        .endpoint = Some(endpoint);
    config
        .routes
        .get_mut("cloud-sota")
        .expect("cloud route")
        .steps = vec![
        RouteTarget::Provider("zai".to_string()),
        RouteTarget::Provider("openrouter".to_string()),
    ];
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("OPENROUTER_API_KEY", "openrouter-test-key");
    let environment_audit = environment.clone();
    let service = FabricGatewayService::from_config(config, environment).expect("service");
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "review the architecture"}],
            "plugins": [{"id": "preserved-plugin", "setting": true}]
        }))
        .expect("JSON"),
    );

    let response = service.handle_chat(&headers(), body).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-provider")
            .and_then(|value| value.to_str().ok()),
        Some("openrouter")
    );
    assert_eq!(
        environment_audit.reads(),
        vec!["OCTOROUTE_API_KEY", "ZAI_API_KEY", "OPENROUTER_API_KEY"]
    );
}

#[tokio::test]
async fn provider_permit_is_held_until_the_streaming_body_is_dropped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer zai-test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"id": "cloud", "model": "glm-5.3", "choices": []})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut config = single_provider_config(&server, "zai");
    config
        .providers
        .get_mut("zai")
        .expect("zai provider")
        .max_in_flight = 1;
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("ZAI_API_KEY", "zai-test-key"),
    )
    .expect("service");
    let request = || {
        Bytes::from(
            serde_json::to_vec(&json!({
                "model": "cloud-sota",
                "messages": [{"role": "user", "content": "review the architecture"}]
            }))
            .expect("JSON"),
        )
    };

    let held = service.handle_chat(&headers(), request()).await;
    assert_eq!(held.status(), 200);
    let busy = service.handle_chat(&headers(), request()).await;
    assert_eq!(busy.status(), 503);
    let body = to_bytes(busy.into_body(), 1024 * 1024)
        .await
        .expect("error body");
    assert!(
        std::str::from_utf8(&body)
            .expect("UTF-8")
            .contains("provider_busy")
    );
    drop(held);
}

#[tokio::test]
async fn v3_models_include_auto_and_all_virtual_routes() {
    let server = MockServer::start().await;
    let service = FabricGatewayService::from_config(
        local_config(&server),
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");
    let models = service.model_ids();
    assert!(models.contains(&"auto".to_string()));
    for model in ["auto-route", "worker", "supervisor", "local", "cloud-sota"] {
        assert!(models.contains(&model.to_string()), "missing {model}");
    }
}
