use super::{FabricConfig, LlamaCppPool, LlamaCppPoolBuildError, PoolAdmissionState, PoolLease};
use crate::gateway::{config::Environment, request::GatewayRequest};
use reqwest::Url;
use serde_json::json;
use std::collections::BTreeSet;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path, query_param},
};

#[derive(Debug, Default)]
struct EmptyEnvironment;

impl Environment for EmptyEnvironment {
    fn get(&self, _name: &str) -> Option<String> {
        None
    }
}

fn example() -> FabricConfig {
    FabricConfig::from_toml(include_str!("../../../config.v3.toml")).expect("repository example")
}

fn request(output_tokens: u32) -> GatewayRequest {
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

fn image_request() -> GatewayRequest {
    GatewayRequest::parse(
        &serde_json::to_vec(&json!({
            "model": "worker",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image_url",
                    "image_url": {"url": "https://example.test/image.png"}
                }]
            }]
        }))
        .expect("request JSON"),
    )
    .expect("valid request")
}

fn worker_pool(servers: &[MockServer]) -> super::LocalPoolConfig {
    let mut pool = example().local_pools["workers"].clone();
    assert_eq!(pool.members.len(), servers.len());
    for (member, server) in pool.members.iter_mut().zip(servers) {
        member.base_url = Url::parse(&server.uri()).expect("mock URL");
    }
    pool
}

async fn mount_ready(server: &MockServer, input_tokens: u32, output_tokens: u32) {
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
            "model": "qwen3.8-27b",
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

fn lease(outcome: super::PoolAdmissionOutcome) -> PoolLease {
    outcome.into_lease().expect("request should be admitted")
}

#[tokio::test]
async fn equal_workers_rotate_across_sequential_sessions() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    for server in &servers {
        mount_ready(server, 20_000, 16_000).await;
    }
    let pool = LlamaCppPool::new(&worker_pool(&servers), &EmptyEnvironment).expect("pool");

    for expected in ["worker-0", "worker-1", "worker-2"] {
        let lease = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
        assert_eq!(lease.member(), expected);
        assert_eq!(lease.pool(), "workers");
        assert_eq!(lease.model_revision(), "unsloth-ud-q4_k_m");
        assert_eq!(lease.chat_url().path(), "/v1/chat/completions");
    }
}

#[tokio::test]
async fn three_held_leases_fill_three_independent_workers() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    for server in &servers {
        mount_ready(server, 10_000, 8_000).await;
    }
    let pool = LlamaCppPool::new(&worker_pool(&servers), &EmptyEnvironment).expect("pool");

    let first = lease(pool.try_admit(&request(8_000)).await.expect("first"));
    let second = lease(pool.try_admit(&request(8_000)).await.expect("second"));
    let third = lease(pool.try_admit(&request(8_000)).await.expect("third"));
    let members = BTreeSet::from([
        first.member().to_string(),
        second.member().to_string(),
        third.member().to_string(),
    ]);
    assert_eq!(members.len(), 3);

    let fourth = pool.try_admit(&request(8_000)).await.expect("fourth");
    assert_eq!(fourth.state(), PoolAdmissionState::Busy);
}

#[tokio::test]
async fn unhealthy_member_is_skipped_before_disclosing_to_next_local_member() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&servers[0])
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .expect(1)
        .mount(&servers[0])
        .await;
    mount_ready(&servers[1], 20_000, 16_000).await;

    let pool = LlamaCppPool::new(&worker_pool(&servers), &EmptyEnvironment).expect("pool");
    let lease = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    assert_eq!(lease.member(), "worker-1");
}

#[tokio::test]
async fn exact_pool_context_budget_rejects_before_chat_dispatch() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    mount_ready(&servers[0], 120_000, 16_000).await;
    let pool = LlamaCppPool::new(&worker_pool(&servers), &EmptyEnvironment).expect("pool");

    let outcome = pool
        .try_admit(&request(16_000))
        .await
        .expect("admission state");
    assert_eq!(outcome.state(), PoolAdmissionState::ContextOverflow);
}

#[tokio::test]
async fn unsupported_request_capability_is_rejected_without_probes() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    let pool = LlamaCppPool::new(&worker_pool(&servers), &EmptyEnvironment).expect("pool");

    let outcome = pool
        .try_admit(&image_request())
        .await
        .expect("admission state");
    assert_eq!(outcome.state(), PoolAdmissionState::Incompatible);
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
