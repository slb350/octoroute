//! Mutation discriminators for local-pool admission and lease safety.

use super::*;
use crate::gateway::fabric::FabricConfig;
use crate::gateway::request::GatewayRequest;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[derive(Debug, Clone, Copy)]
struct EmptyEnvironment;

impl Environment for EmptyEnvironment {
    fn get(&self, _name: &str) -> Option<SecretString> {
        None
    }
}

fn config() -> FabricConfig {
    FabricConfig::from_toml(include_str!("../../../../config.toml")).expect("repository config")
}

fn request(output_tokens: u32) -> GatewayRequest {
    GatewayRequest::parse(
        &serde_json::to_vec(&json!({
            "model": "worker",
            "messages": [{"role": "user", "content": "bounded request"}],
            "max_completion_tokens": output_tokens
        }))
        .expect("request JSON"),
    )
    .expect("gateway request")
}

#[tokio::test]
async fn round_robin_cursor_wraps_before_rotation_continues() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    for server in &servers {
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/slots"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])),
            )
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions/input_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 1})))
            .mount(server)
            .await;
    }

    let mut pool_config = config().local_pools["workers"].clone();
    for (member, server) in pool_config.members.iter_mut().zip(&servers) {
        member.base_url = Url::parse(&server.uri()).expect("mock URL");
    }
    let pool = LlamaCppPool::new(&pool_config, &EmptyEnvironment).expect("pool");

    for expected in ["worker-0", "worker-1", "worker-2", "worker-0"] {
        let outcome = pool.try_admit(&request(1_024)).await.expect("admission");
        let PoolAdmissionOutcome::Admitted(lease) = outcome else {
            panic!("member {expected} must admit")
        };
        assert_eq!(lease.member(), expected);
        drop(lease);
    }

    assert_eq!(
        pool.inner.cursor.load(Ordering::Relaxed),
        1,
        "the cursor must stay bounded after wrapping past the last member"
    );
}

#[tokio::test]
async fn pool_lease_debug_names_safe_routing_fields_and_redacts_sensitive_fields() {
    let permit = Arc::new(Semaphore::new(1))
        .acquire_owned()
        .await
        .expect("permit");
    let lease = PoolLease {
        pool: "workers".to_string(),
        member: "worker-0".to_string(),
        model_revision: "revision-safe".to_string(),
        chat_url: Url::parse("http://127.0.0.1/v1/chat/completions").expect("URL"),
        api_key: Some(SecretString::from("member-secret".to_string())),
        request_body: Bytes::from_static(b"prompt-secret"),
        deadlines: UpstreamDeadlines::new(1_000, None),
        _permit: permit,
    };

    let debug = format!("{lease:?}");

    for visible in ["PoolLease", "workers", "worker-0", "revision-safe"] {
        assert!(
            debug.contains(visible),
            "missing safe field `{visible}`: {debug}"
        );
    }
    assert_eq!(debug.matches("[REDACTED]").count(), 2, "{debug}");
    for secret in ["member-secret", "prompt-secret"] {
        assert!(!debug.contains(secret), "debug leaked `{secret}`: {debug}");
    }
}

#[tokio::test]
async fn exact_total_context_use_is_admitted() {
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

    let output_tokens = 1_024;
    let mut pool_config = config().local_pools["workers"].clone();
    pool_config.members.truncate(1);
    pool_config.members[0].base_url = Url::parse(&server.uri()).expect("mock URL");
    let input_tokens =
        pool_config.context_window - pool_config.context_safety_tokens - output_tokens;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"input_tokens": input_tokens})),
        )
        .mount(&server)
        .await;
    let pool = LlamaCppPool::new(&pool_config, &EmptyEnvironment).expect("pool");

    let outcome = pool
        .try_admit(&request(output_tokens))
        .await
        .expect("admission");

    assert_eq!(outcome.state(), PoolAdmissionState::Ready);
}

#[test]
fn readiness_token_probe_body_is_the_fixed_minimal_chat() {
    let pool_config = config().local_pools["workers"].clone();
    let pool = LlamaCppPool::new(&pool_config, &EmptyEnvironment).expect("pool");

    let body: Value = serde_json::from_slice(&pool.token_count_probe_body()).expect("probe JSON");

    assert_eq!(
        body,
        json!({
            "model": "coding-worker-model",
            "messages": [{"role": "user", "content": "ping"}]
        })
    );
}

#[tokio::test]
async fn fully_reserved_members_are_not_admission_candidates() {
    let pool_config = config().local_pools["workers"].clone();
    let pool = LlamaCppPool::new(&pool_config, &EmptyEnvironment).expect("pool");
    let reserved = Arc::clone(&pool.inner.members[0].permits)
        .acquire_many_owned(pool.inner.members[0].max_in_flight as u32)
        .await
        .expect("reserve member");

    let candidates = pool.candidates();

    assert_eq!(candidates.len(), pool.inner.members.len() - 1);
    assert!(candidates.iter().all(|(index, _)| *index != 0));
    drop(reserved);
}
