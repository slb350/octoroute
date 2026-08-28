//! Shared fixtures for the local-pool tests.
//!
//! Topic-focused cases live in the sibling `local_pool_*_tests` modules; this
//! file owns the configuration, request, and mock-server helpers they share,
//! plus the pool construction and credential checks that run before any probe.

use super::{FabricConfig, LlamaCppPool, LlamaCppPoolBuildError, PoolLease};
use crate::gateway::{env::Environment, request::GatewayRequest};
use reqwest::Url;
use secrecy::SecretString;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path, query_param},
};

#[derive(Debug, Default)]
pub(super) struct EmptyEnvironment;

impl Environment for EmptyEnvironment {
    fn get(&self, _name: &str) -> Option<SecretString> {
        None
    }
}

struct InvalidCredentialEnvironment;

impl Environment for InvalidCredentialEnvironment {
    fn get(&self, _name: &str) -> Option<SecretString> {
        Some(SecretString::from("invalid credential\n"))
    }
}

pub(super) fn example() -> FabricConfig {
    FabricConfig::from_toml(include_str!("../../../config.toml")).expect("repository example")
}

pub(super) fn request(output_tokens: u32) -> GatewayRequest {
    GatewayRequest::parse(
        &serde_json::to_vec(&json!({
            "model": "worker",
            "messages": [{"role": "user", "content": "implement the bounded task"}],
            "max_completion_tokens": output_tokens,
            "reasoning_effort": "medium"
        }))
        .expect("request JSON"),
    )
    .expect("valid request")
}

pub(super) fn worker_pool(servers: &[MockServer]) -> super::LocalPoolConfig {
    let mut pool = example().local_pools["workers"].clone();
    assert_eq!(pool.members.len(), servers.len());
    for (member, server) in pool.members.iter_mut().zip(servers) {
        member.base_url = Url::parse(&server.uri()).expect("mock URL");
    }
    pool
}

pub(super) async fn mount_ready(server: &MockServer, input_tokens: u32, output_tokens: u32) {
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
        .and(body_json(json!({
            "model": "coding-worker-model",
            "messages": [{"role": "user", "content": "implement the bounded task"}],
            "max_completion_tokens": output_tokens,
            "reasoning_effort": "medium"
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"input_tokens": input_tokens})),
        )
        .expect(1)
        .mount(server)
        .await;
}

/// A one-member pool pointed at `server`.
///
/// Most probe and classification cases turn on what a single member answers, and
/// a lone member also removes the question of which member a verdict came from.
pub(super) fn single_member_pool(server: &MockServer) -> super::LocalPoolConfig {
    let mut pool = example().local_pools["workers"].clone();
    pool.members.truncate(1);
    pool.members[0].base_url = Url::parse(&server.uri()).expect("mock URL");
    pool
}

/// Mount `/health` and `/slots` as an idle, healthy member.
///
/// Call counts are left unpinned: callers that care about them mount their own.
pub(super) async fn mount_probes_ready(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .mount(server)
        .await;
}

/// As [`mount_ready`], without pinning per-server call counts.
///
/// Selection tests dispatch to one member of several, so the members that lose
/// the selection legitimately receive probes but no token count.
pub(super) async fn mount_available(server: &MockServer, input_tokens: u32) {
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"input_tokens": input_tokens})),
        )
        .mount(server)
        .await;
}

pub(super) fn lease(outcome: super::PoolAdmissionOutcome) -> PoolLease {
    outcome.into_lease().expect("request should be admitted")
}

#[test]
fn referenced_local_member_secret_must_exist_at_startup() {
    let mut pool = example().local_pools["workers"].clone();
    pool.members[0].api_key_env = Some("MISSING_WORKER_KEY".to_string());

    let error = match LlamaCppPool::new(&pool, &EmptyEnvironment) {
        Ok(_) => panic!("missing secret must fail startup"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        LlamaCppPoolBuildError::MissingEnvironmentVariable { .. }
    ));
}

#[test]
fn referenced_local_member_secret_must_be_header_safe_at_startup() {
    let mut pool = example().local_pools["workers"].clone();
    pool.members[0].api_key_env = Some("WORKER_KEY".to_string());

    let error = match LlamaCppPool::new(&pool, &InvalidCredentialEnvironment) {
        Ok(_) => panic!("invalid secret must fail startup"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        LlamaCppPoolBuildError::InvalidCredential { .. }
    ));
}
