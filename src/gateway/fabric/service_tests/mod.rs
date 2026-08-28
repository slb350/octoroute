use super::{
    FabricConfig, FabricGatewayService, FallbackTrigger, ProviderAdmissionState,
    ProviderRuntimeConfig, RouteTarget,
};
use crate::gateway::env::Environment;
use axum::{
    body::{Body, Bytes, to_bytes},
    http::{HeaderMap, HeaderValue, Response, header::AUTHORIZATION},
};
use reqwest::Url;
use secrecy::SecretString;
use serde_json::{Value, json};
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
    fn get(&self, name: &str) -> Option<SecretString> {
        self.reads
            .lock()
            .expect("reads mutex")
            .push(name.to_string());
        self.values.get(name).cloned().map(SecretString::from)
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

/// The shared cloud-route request body used by the provider dispatch tests.
fn cloud_request() -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "review the architecture"}],
            "plugins": [{"id": "preserved-plugin", "setting": true}]
        }))
        .expect("JSON"),
    )
}

async fn response_body(response: Response<Body>) -> Bytes {
    to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("response body")
}

/// Repoint an HTTP provider at a mock server, preserving its protocol and
/// credential source.
fn set_provider_endpoint(config: &mut FabricConfig, provider: &str, endpoint: Url) {
    let runtime = &mut config
        .providers
        .get_mut(provider)
        .expect("configured provider")
        .runtime;
    match runtime {
        ProviderRuntimeConfig::Http {
            protocol,
            credential,
            ..
        } => {
            *runtime = ProviderRuntimeConfig::Http {
                endpoint,
                protocol: *protocol,
                credential: credential.clone(),
            };
        }
        ProviderRuntimeConfig::CodexCli { .. } => panic!("{provider} is not an HTTP provider"),
    }
}

fn local_config(server: &MockServer) -> FabricConfig {
    let mut config =
        FabricConfig::from_toml(include_str!("../../../../config.toml")).expect("config");
    let workers = config.local_pools.get_mut("workers").expect("workers pool");
    workers.members.truncate(1);
    workers.members[0].base_url = Url::parse(&server.uri()).expect("mock URL");
    config
}

fn single_provider_config(server: &MockServer, provider: &str) -> FabricConfig {
    let mut config = local_config(server);
    let endpoint = Url::parse(&format!("{}/", server.uri())).expect("mock provider URL");
    set_provider_endpoint(&mut config, provider, endpoint);
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

mod codex;
mod local;
mod provider;
