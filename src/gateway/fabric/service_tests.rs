use super::{
    FabricConfig, FabricGatewayService, FallbackTrigger, ProviderAdmissionState, RouteTarget,
};
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
async fn incompatible_codex_request_fails_closed_without_launching_the_cli() {
    let server = MockServer::start().await;
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("OPENROUTER_API_KEY", "unused-openrouter-key");
    let environment_audit = environment.clone();
    let service =
        FabricGatewayService::from_config(local_config(&server), environment).expect("service");
    let incompatible_requests = [
        json!({
            "model": "cloud-sota",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image_url",
                    "image_url": {"url": "data:image/png;base64,AA=="}
                }]
            }]
        }),
        json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "two answers"}],
            "n": 2
        }),
    ];

    for request in incompatible_requests {
        let body = Bytes::from(serde_json::to_vec(&request).expect("JSON"));
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
    }
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
    let metrics = service.metrics_text();
    assert!(metrics.contains(
        "octoroute_fabric_provider_admissions_total{provider=\"zai\",state=\"unavailable\"} 1"
    ));
    assert!(metrics.contains(
        "octoroute_fabric_provider_fallbacks_total{provider=\"zai\",trigger=\"unhealthy\"} 1"
    ));
    assert!(metrics.contains(
        "octoroute_fabric_provider_responses_total{provider=\"openrouter\",outcome=\"success\"} 1"
    ));
}

#[tokio::test]
async fn provider_response_fallback_obeys_the_closed_trigger_set() {
    for (first_status, remove_trigger, expected_status, expected_provider, falls_forward) in [
        (
            429,
            None,
            200,
            "openrouter",
            true,
        ),
        (
            429,
            Some(FallbackTrigger::RateLimited),
            429,
            "zai",
            false,
        ),
        (
            503,
            None,
            200,
            "openrouter",
            true,
        ),
        (
            401,
            None,
            401,
            "zai",
            false,
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer zai-test-key"))
            .respond_with(
                ResponseTemplate::new(first_status)
                    .insert_header("content-type", "application/json")
                    .set_body_json(json!({"error": {"message": "bounded fixture"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer openrouter-test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(json!({"id": "fallback", "choices": []})),
            )
            .expect(if falls_forward { 1 } else { 0 })
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
        let route = config.routes.get_mut("cloud-sota").expect("cloud route");
        route.steps = vec![
            RouteTarget::Provider("zai".to_string()),
            RouteTarget::Provider("openrouter".to_string()),
        ];
        if let Some(trigger) = remove_trigger {
            route.fallback_on.remove(&trigger);
        }
        let service = FabricGatewayService::from_config(
            config,
            TestEnvironment::default()
                .with("OCTOROUTE_API_KEY", "inbound-test-key")
                .with("ZAI_API_KEY", "zai-test-key")
                .with("OPENROUTER_API_KEY", "openrouter-test-key"),
        )
        .expect("service");
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "cloud-sota",
                "messages": [{"role": "user", "content": "review the boundary"}]
            }))
            .expect("JSON"),
        );

        let response = service.handle_chat(&headers(), body).await;
        assert_eq!(response.status().as_u16(), expected_status);
        assert_eq!(
            response
                .headers()
                .get("x-octoroute-provider")
                .and_then(|value| value.to_str().ok()),
            Some(expected_provider)
        );
        drop(response);
    }
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

#[tokio::test]
async fn provider_readiness_probes_auth_once_per_cache_window() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", "Bearer zai-test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .expect(1)
        .mount(&server)
        .await;

    let mut config = single_provider_config(&server, "zai");
    for pool in config.local_pools.values_mut() {
        pool.enabled = false;
    }
    for (name, provider) in &mut config.providers {
        provider.enabled = name == "zai";
    }
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("ZAI_API_KEY", "zai-test-key");
    let environment_audit = environment.clone();
    let service = FabricGatewayService::from_config(config, environment).expect("service");

    for _ in 0..2 {
        let readiness = service.readiness().await;
        assert_eq!(
            readiness.providers().get("zai"),
            Some(&ProviderAdmissionState::Ready)
        );
    }
    assert_eq!(
        environment_audit.reads(),
        vec!["OCTOROUTE_API_KEY", "ZAI_API_KEY"]
    );
    assert!(
        service
            .metrics_text()
            .contains("octoroute_fabric_provider_probes_total{provider=\"zai\",state=\"ready\"} 1")
    );
}

#[tokio::test]
async fn opencode_style_anthropic_tools_stream_as_open_ai_chunks() {
    let server = MockServer::start().await;
    let expected_request = json!({
        "model": "k3",
        "messages": [{
            "role": "user",
            "content": [{"type": "text", "text": "inspect the repository"}]
        }],
        "max_tokens": 200000,
        "stream": true,
        "thinking": {"type": "enabled", "budget_tokens": 16384},
        "tools": [{
            "name": "read_file",
            "description": "Read one repository file",
            "input_schema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }
        }],
        "tool_choice": {"type": "auto"}
    });
    let sse = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-kimi\",\"model\":\"k3\"}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call-1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"src/main.rs\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":12}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/messages"))
        .and(header("x-api-key", "kimi-test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(body_json(expected_request))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(sse, "text/event-stream"),
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
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "inspect the repository"}],
            "stream": true,
            "reasoning_effort": "high",
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read one repository file",
                    "parameters": {
                        "type": "object",
                        "properties": {"path": {"type": "string"}},
                        "required": ["path"]
                    }
                }
            }],
            "tool_choice": "auto"
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
        Some("kimi")
    );
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("translated stream");
    let body = std::str::from_utf8(&body).expect("UTF-8");
    assert!(body.contains("chat.completion.chunk"), "{body}");
    assert!(body.contains("tool_calls"), "{body}");
    assert!(body.contains("src/main.rs"), "{body}");
    assert!(body.contains("data: [DONE]"), "{body}");
}

#[cfg(unix)]
#[tokio::test]
async fn codex_cli_dispatch_is_ephemeral_filtered_and_open_ai_compatible() {
    use std::os::unix::fs::PermissionsExt as _;

    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary Codex fixture");
    let executable = directory.path().join("fake-codex");
    std::fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "if [ \"${1:-}\" = doctor ]; then\n",
            "  printf '%s' '{\"schemaVersion\":1,\"codexVersion\":\"0.148.0\",\"checks\":{\"auth.credentials\":{\"details\":{\"stored ChatGPT tokens\":\"true\",\"stored auth mode\":\"chatgpt\"}}}}'\n",
            "  exit 0\n",
            "fi\n",
            "sed -n '1,$p' >/dev/null\n",
            "printf '%s\\n' \\\n",
            "  '{\"type\":\"thread.started\",\"thread_id\":\"redacted\"}' \\\n",
            "  '{\"type\":\"turn.started\"}' \\\n",
            "  '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"{\\\"content\\\":\\\"Codex answer\\\",\\\"reasoning_content\\\":null,\\\"tool_calls\\\":[],\\\"finish_reason\\\":\\\"stop\\\"}\"}}' \\\n",
            "  '{\"type\":\"turn.completed\"}'\n"
        ),
    )
    .expect("fake Codex executable");
    let mut permissions = std::fs::metadata(&executable)
        .expect("fake Codex metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).expect("fake Codex permissions");

    let mut config = local_config(&server);
    for provider in config.providers.values_mut() {
        provider.enabled = false;
    }
    let codex = config.providers.get_mut("codex").expect("codex provider");
    codex.enabled = true;
    codex.executable = Some(executable.to_string_lossy().into_owned());
    config
        .routes
        .get_mut("cloud-sota")
        .expect("cloud route")
        .steps = vec![RouteTarget::Provider("codex".to_string())];
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "review this change"}]
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
        Some("codex")
    );
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("Codex response");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("OpenAI JSON response");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "Codex answer");

    let readiness = service.readiness().await;
    assert_eq!(
        readiness.providers().get("codex"),
        Some(&ProviderAdmissionState::Ready)
    );
}
