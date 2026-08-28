//! Member probing, endpoint resolution, and capability-driven body shaping.

use super::local_pool_tests::{
    EmptyEnvironment, example, lease, mount_probes_ready, request, single_member_pool, worker_pool,
};
use super::{LlamaCppPool, LocalCapability, PoolAdmissionState};
use crate::gateway::request::GatewayRequest;
use reqwest::Url;
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path},
};

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

fn request_without_reasoning(output_tokens: u32) -> GatewayRequest {
    GatewayRequest::parse(
        &serde_json::to_vec(&json!({
            "model": "worker",
            "messages": [{"role": "user", "content": "implement the bounded task"}],
            "max_completion_tokens": output_tokens
        }))
        .expect("request JSON"),
    )
    .expect("valid request")
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

/// A pool that never declared `reasoning` must not have its
/// `default_reasoning_effort` injected: the body llama.cpp receives would then
/// carry a control the operator never configured the model to accept.
#[tokio::test]
async fn pool_without_the_reasoning_capability_does_not_inject_its_default() {
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
        .and(body_json(json!({
            "model": "coding-worker-model",
            "messages": [{"role": "user", "content": "implement the bounded task"}],
            "max_completion_tokens": 16_000
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 20_000})))
        .expect(1)
        .mount(&server)
        .await;

    let mut config = example().local_pools["workers"].clone();
    config.members.truncate(1);
    config.members[0].base_url = Url::parse(&server.uri()).expect("mock URL");
    assert!(config.capabilities.remove(&LocalCapability::Reasoning));
    let pool = LlamaCppPool::new(&config, &EmptyEnvironment).expect("pool");

    let lease = lease(
        pool.try_admit(&request_without_reasoning(16_000))
            .await
            .expect("admission"),
    );
    assert_eq!(lease.member(), "worker-0");
}

/// A base URL whose path has no trailing slash must keep that path: `Url::join`
/// treats the last segment as a file and drops it, which would silently send
/// every probe to the wrong prefix.
#[tokio::test]
async fn member_endpoints_resolve_under_a_base_path_without_a_trailing_slash() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/llama/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/llama/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/llama/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 20_000})))
        .expect(1)
        .mount(&server)
        .await;

    let mut config = example().local_pools["workers"].clone();
    config.members.truncate(1);
    config.members[0].base_url = Url::parse(&format!("{}/llama", server.uri())).expect("mock URL");
    let pool = LlamaCppPool::new(&config, &EmptyEnvironment).expect("pool");

    let lease = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    assert_eq!(lease.chat_url().path(), "/llama/v1/chat/completions");
}

/// Probe bodies are upstream-controlled. A member that answers `/health` with
/// megabytes is not a member this code can use, and must not be buffered whole.
#[tokio::test]
async fn oversized_probe_body_is_refused_rather_than_buffered() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "ok", "pad": "x".repeat(2 * 1024 * 1024)})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .mount(&server)
        .await;

    let pool = LlamaCppPool::new(&single_member_pool(&server), &EmptyEnvironment).expect("pool");

    assert_eq!(pool.readiness_state().await, PoolAdmissionState::Unhealthy);
}

/// The ceiling is per call site, not per module: `/slots` is the largest probe
/// body llama.cpp produces and so the one most likely to be read unbounded.
#[tokio::test]
async fn an_oversized_slots_body_is_refused_rather_than_buffered() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!([{"is_processing": false, "pad": "x".repeat(2 * 1024 * 1024)}]),
            ),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 20_000})))
        .expect(0)
        .mount(&server)
        .await;

    let pool = LlamaCppPool::new(&single_member_pool(&server), &EmptyEnvironment).expect("pool");

    let outcome = pool.try_admit(&request(16_000)).await.expect("admission");
    assert_eq!(outcome.state(), PoolAdmissionState::Unhealthy);
}

/// The token count decides whether a prompt fits the context window, so an
/// answer too large to read is no answer, not a number to trust.
#[tokio::test]
async fn an_oversized_token_count_body_is_refused_rather_than_buffered() {
    let server = MockServer::start().await;
    mount_probes_ready(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"input_tokens": 20_000, "pad": "x".repeat(2 * 1024 * 1024)})),
        )
        .mount(&server)
        .await;

    let pool = LlamaCppPool::new(&single_member_pool(&server), &EmptyEnvironment).expect("pool");

    let outcome = pool.try_admit(&request(16_000)).await.expect("admission");
    assert_eq!(outcome.state(), PoolAdmissionState::TokenCountUnavailable);
}

/// The ceiling has to clear a real `/slots` answer. llama.cpp emits about 1.1 KB
/// per slot, and a member serving 64 of them is ordinary, so a probe body of
/// several hundred kilobytes is a member to admit rather than one to refuse.
#[tokio::test]
async fn a_large_probe_body_under_the_ceiling_is_still_admitted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "ok", "pad": "x".repeat(512 * 1024)})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 20_000})))
        .mount(&server)
        .await;

    let pool = LlamaCppPool::new(&single_member_pool(&server), &EmptyEnvironment).expect("pool");

    let lease = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    assert_eq!(lease.member(), "worker-0");
}

/// `/health` is cached for a second so that a burst of admissions costs one
/// probe. Without the cache write every admission re-probes, which is invisible
/// in the verdict and only shows up as load on the member.
#[tokio::test]
async fn a_fresh_health_result_is_reused_rather_than_reprobed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 20_000})))
        .expect(2)
        .mount(&server)
        .await;

    let pool = LlamaCppPool::new(&single_member_pool(&server), &EmptyEnvironment).expect("pool");

    let first = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    drop(first);
    let second = lease(pool.try_admit(&request(16_000)).await.expect("admission"));
    assert_eq!(second.member(), "worker-0");
}

/// A `/health` body is only an answer when the status says so. Reading the body
/// of a failed response would let a member that returns 500 with a cached or
/// proxied `{"status":"ok"}` present itself as healthy.
#[tokio::test]
async fn a_health_body_is_only_believed_on_a_success_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"status": "ok"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 20_000})))
        .expect(0)
        .mount(&server)
        .await;

    let pool = LlamaCppPool::new(&single_member_pool(&server), &EmptyEnvironment).expect("pool");

    let outcome = pool.try_admit(&request(16_000)).await.expect("admission");
    assert_eq!(outcome.state(), PoolAdmissionState::Unhealthy);
}

/// llama.cpp answers `/slots?fail_on_no_slot=1` with 503 when every slot is
/// taken. That is capacity, and it has to reach `busy`: reporting it as
/// ill-health records the wrong trigger on the one metric that says local
/// capacity is spilling to cloud, and falls forward on the wrong routes.
#[tokio::test]
async fn a_slots_probe_answering_503_is_busy_rather_than_unhealthy() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let pool = LlamaCppPool::new(&single_member_pool(&server), &EmptyEnvironment).expect("pool");

    let outcome = pool.try_admit(&request(16_000)).await.expect("admission");
    assert_eq!(outcome.state(), PoolAdmissionState::Busy);
}

/// A member with no slots at all is not a busy member: nothing it is serving
/// will ever finish and free one, so waiting on it is the wrong answer.
#[tokio::test]
async fn an_empty_slots_array_is_unhealthy_rather_than_busy() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;

    let pool = LlamaCppPool::new(&single_member_pool(&server), &EmptyEnvironment).expect("pool");

    let outcome = pool.try_admit(&request(16_000)).await.expect("admission");
    assert_eq!(outcome.state(), PoolAdmissionState::Unhealthy);
}
