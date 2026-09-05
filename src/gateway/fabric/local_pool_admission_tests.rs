//! Token counting and context-window admission.

use super::local_pool_tests::{
    EmptyEnvironment, mount_probes_ready, mount_ready, request, single_member_pool, worker_pool,
};
use super::{LlamaCppPool, PoolAdmissionState};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

/// Mount health and slots as ready, and assert the token-count endpoint is
/// never called.
async fn mount_ready_but_never_counted(server: &MockServer) {
    mount_probes_ready(server).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 1})))
        .expect(0)
        .mount(server)
        .await;
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

/// When the output reservation alone fills the context window, no token count
/// can change the verdict, so no member is probed and no prompt is disclosed.
#[tokio::test]
async fn deterministic_context_overflow_rejects_before_any_member_is_probed() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    for server in &servers {
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
            .expect(0)
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/slots"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([{"is_processing": false}])),
            )
            .expect(0)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions/input_tokens"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 1})))
            .expect(0)
            .mount(server)
            .await;
    }
    let config = worker_pool(&servers);
    // context_window - context_safety_tokens leaves nothing for the prompt, and
    // a non-empty prompt is at least one token.
    let output_tokens = config.context_window - config.context_safety_tokens;
    let pool = LlamaCppPool::new(&config, &EmptyEnvironment).expect("pool");

    let outcome = pool
        .try_admit(&request(output_tokens))
        .await
        .expect("admission state");

    assert_eq!(outcome.state(), PoolAdmissionState::ContextOverflow);
}

/// `/v1/chat/completions/input_tokens` applies the chat template, so a 400 is a
/// verdict on the request. Every equivalent member would answer the same way:
/// retrying discloses the prompt again for nothing, and reporting it as member
/// ill-health spills it to a paid provider on a `cloud_allowed` route.
#[tokio::test]
async fn token_count_request_rejection_is_not_retried_against_the_next_member() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    mount_probes_ready(&servers[0]).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "template"})))
        .expect(1)
        .mount(&servers[0])
        .await;
    mount_ready_but_never_counted(&servers[1]).await;
    mount_ready_but_never_counted(&servers[2]).await;

    let pool = LlamaCppPool::new(&worker_pool(&servers), &EmptyEnvironment).expect("pool");
    let outcome = pool
        .try_admit(&request(16_000))
        .await
        .expect("admission state");

    assert_eq!(outcome.state(), PoolAdmissionState::Incompatible);
}

/// A 5xx or a missing endpoint still describes the member, so the next member
/// is tried and the pool reports the token count as unavailable rather than
/// blaming the request.
#[tokio::test]
async fn token_count_server_error_still_falls_through_to_the_next_member() {
    for status in [500, 404] {
        let servers = [
            MockServer::start().await,
            MockServer::start().await,
            MockServer::start().await,
        ];
        for server in &servers {
            mount_probes_ready(server).await;
            Mock::given(method("POST"))
                .and(path("/v1/chat/completions/input_tokens"))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(server)
                .await;
        }

        let pool = LlamaCppPool::new(&worker_pool(&servers), &EmptyEnvironment).expect("pool");
        let outcome = pool
            .try_admit(&request(16_000))
            .await
            .expect("admission state");

        assert_eq!(
            outcome.state(),
            PoolAdmissionState::TokenCountUnavailable,
            "status {status} describes the member, not the request"
        );
    }
}

/// Every status the token-count classifier names reaches a state no other status
/// in the list reaches, so dropping an arm cannot pass unnoticed. The classes
/// are deliberately different: a credential rejection has to surface, load has
/// to fall forward on `busy`, a timeout is a transport failure, an absent
/// endpoint is unavailability, and anything else 4xx is a verdict on the body.
#[tokio::test]
async fn each_token_count_status_class_reaches_its_own_admission_state() {
    for (status, expected) in [
        (400, PoolAdmissionState::Incompatible),
        (401, PoolAdmissionState::Unauthenticated),
        (403, PoolAdmissionState::Unauthenticated),
        (404, PoolAdmissionState::TokenCountUnavailable),
        (407, PoolAdmissionState::Unauthenticated),
        (408, PoolAdmissionState::Unhealthy),
        (422, PoolAdmissionState::Incompatible),
        (429, PoolAdmissionState::Busy),
        (500, PoolAdmissionState::TokenCountUnavailable),
        (501, PoolAdmissionState::TokenCountUnavailable),
    ] {
        let server = MockServer::start().await;
        mount_probes_ready(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions/input_tokens"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        let pool =
            LlamaCppPool::new(&single_member_pool(&server), &EmptyEnvironment).expect("pool");

        let outcome = pool.try_admit(&request(16_000)).await.expect("admission");

        assert_eq!(outcome.state(), expected, "token count answered {status}");
    }
}

/// Readiness has to separate the same classes as admission. A pool whose members
/// all reject the configured credential must not report as merely unhealthy on
/// `/health/ready`, which is the operator's only sight of the condition.
#[tokio::test]
async fn each_token_count_status_class_reaches_its_own_readiness_state() {
    for (status, expected) in [
        (401, PoolAdmissionState::Unauthenticated),
        (408, PoolAdmissionState::Unhealthy),
        (429, PoolAdmissionState::Busy),
        (404, PoolAdmissionState::TokenCountUnavailable),
    ] {
        let server = MockServer::start().await;
        mount_probes_ready(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions/input_tokens"))
            .respond_with(ResponseTemplate::new(status))
            .expect(1)
            .mount(&server)
            .await;
        let pool =
            LlamaCppPool::new(&single_member_pool(&server), &EmptyEnvironment).expect("pool");

        assert_eq!(
            pool.readiness_state().await,
            expected,
            "token count answered {status}"
        );
    }
}

/// Every member of a pool is configured from the same environment, so a rotated
/// key fails on all of them. Trying the rest turns one visible credential error
/// into a pool that merely looks sick, and `unhealthy` is in the default
/// fallback set: the prompt and the spend go to a paid provider silently.
#[tokio::test]
async fn token_count_credential_rejection_is_not_retried_against_the_next_member() {
    let servers = [
        MockServer::start().await,
        MockServer::start().await,
        MockServer::start().await,
    ];
    mount_probes_ready(&servers[0]).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&servers[0])
        .await;
    mount_ready_but_never_counted(&servers[1]).await;
    mount_ready_but_never_counted(&servers[2]).await;

    let pool = LlamaCppPool::new(&worker_pool(&servers), &EmptyEnvironment).expect("pool");
    let outcome = pool.try_admit(&request(16_000)).await.expect("admission");

    assert_eq!(outcome.state(), PoolAdmissionState::Unauthenticated);
}
