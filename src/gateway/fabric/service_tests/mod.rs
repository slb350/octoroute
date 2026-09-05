use super::{
    FabricConfig, FabricGatewayService, FallbackTrigger, ProviderAdmissionState,
    ProviderRuntimeConfig, RoutePrivacy, RouteTarget,
};
use crate::gateway::env::Environment;
use axum::{
    body::{Body, Bytes, to_bytes},
    http::{HeaderMap, HeaderValue, Request, Response, header::AUTHORIZATION},
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

/// Point one provider at `server` and disable every other readiness target.
fn single_enabled_provider_config(server: &MockServer, provider: &str) -> FabricConfig {
    let mut config = single_provider_config(server, provider);
    for pool in config.local_pools.values_mut() {
        pool.enabled = false;
    }
    for (name, configured) in &mut config.providers {
        configured.enabled = name == provider;
    }
    config
}

/// Mount the probes one local admission runs against a member that admits:
/// healthy, one free slot, and a bounded token count.
///
/// The dispatch response is left to the caller, so a test can decide what the
/// member does *after* it has been admitted.
async fn mount_local_admission(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .and(query_param("fail_on_no_slot", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 128})))
        .mount(server)
        .await;
}

/// The shared local-route request body used by the dispatch tests.
fn local_request(model: &str) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "implement the bounded task"}]
        }))
        .expect("JSON"),
    )
}

mod codex;
mod commit_boundary;
mod credential;
mod local;
mod preflight;
mod provider;
mod readiness;
mod redirect;
/// A protocol-neutral cloud body accepted by every provider adapter.
fn portable_cloud_request() -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "review the architecture"}]
        }))
        .expect("JSON"),
    )
}
