use super::{FabricConfig, LlamaCppPool, LlamaCppPoolBuildError, PoolAdmissionState, PoolLease};
use crate::gateway::{env::Environment, request::GatewayRequest};
use reqwest::Url;
use secrecy::SecretString;
use serde_json::json;
use std::collections::BTreeSet;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path, query_param},
};

#[derive(Debug, Default)]
struct EmptyEnvironment;

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

fn example() -> FabricConfig {
    FabricConfig::from_toml(include_str!("../../../config.toml")).expect("repository example")
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

/// As [`mount_ready`], without pinning per-server call counts.
///
/// Selection tests dispatch to one member of several, so the members that lose
/// the selection legitimately receive probes but no token count.
async fn mount_available(server: &MockServer, input_tokens: u32) {
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
        assert_eq!(lease.model_revision(), "example-worker-revision");
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

/// Member `priority` sorts ascending: a lower number is preferred. This is the
/// production selection path, not a parallel copy of its ordering rules.
#[tokio::test]
async fn lower_priority_number_is_preferred_over_rotation() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    for server in &servers {
        mount_available(server, 20_000).await;
    }
    let mut config = worker_pool(&servers);
    // worker-2 is the least preferred by rotation and the most preferred by
    // priority, so only priority can explain selecting it first.
    config.members[0].priority = 100;
    config.members[1].priority = 100;
    config.members[2].priority = 10;
    let pool = LlamaCppPool::new(&config, &EmptyEnvironment).expect("pool");

    let lease = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    assert_eq!(lease.member(), "worker-2");
}

/// A member whose token-count endpoint stops answering is distinguishable from
/// an unreachable one, and readiness reports it rather than claiming `ready`.
#[tokio::test]
async fn missing_token_count_endpoint_is_reported_rather_than_silently_ready() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let mut config = example().local_pools["workers"].clone();
    config.members.truncate(1);
    config.members[0].base_url = Url::parse(&server.uri()).expect("mock URL");
    let pool = LlamaCppPool::new(&config, &EmptyEnvironment).expect("pool");

    assert_eq!(
        pool.readiness_state().await,
        super::PoolAdmissionState::TokenCountUnavailable
    );
    assert!(matches!(
        pool.try_admit(&request(16_000)).await.expect("admission"),
        super::PoolAdmissionOutcome::Rejected(super::PoolAdmissionState::TokenCountUnavailable)
    ));
}

/// Least-loaded selection: a member already serving a request must lose to an
/// idle one even when rotation would pick it first.
///
/// Rotation has to point AT the busy member for this to discriminate. `try_admit`
/// advances the cursor past whatever it just picked, so the sequence below walks
/// the cursor all the way around back to the member that is still holding a
/// lease. Deleting the in-flight term from the sort key in `candidates` makes
/// rotation decide, and this test then fails.
#[tokio::test]
async fn busier_member_loses_to_an_idle_one_even_when_rotation_favours_it() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    for server in &servers {
        mount_available(server, 20_000).await;
    }
    // worker-0 keeps spare capacity so it stays selectable while holding a lease;
    // load, not capacity, is what must exclude it.
    let mut config = worker_pool(&servers);
    config.members[0].max_in_flight = 3;
    let pool = LlamaCppPool::new(&config, &EmptyEnvironment).expect("pool");

    let held = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    assert_eq!(held.member(), "worker-0", "cursor starts at worker-0");

    // Walk the cursor back around to worker-0, releasing each lease so only
    // worker-0 is left carrying load.
    for expected in ["worker-1", "worker-2"] {
        let transient = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
        assert_eq!(transient.member(), expected);
        drop(transient);
    }

    // Rotation now favours worker-0 and load does not.
    let selected = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    assert_ne!(
        selected.member(),
        "worker-0",
        "an idle member must win over one already serving a request"
    );
    drop(held);
}
