use super::{
    config::GatewayConfig,
    local::{AdmissionOutcome, LlamaCppAdmission},
    request::GatewayRequest,
    routing::LocalAdmissionState,
    test_support::gateway_config,
};
use serde_json::json;
use std::time::{Duration, Instant};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, method, path, query_param},
};

fn config(server: &MockServer, extra_local: &str) -> GatewayConfig {
    gateway_config(&server.uri(), extra_local, "", "")
}

fn request(max_completion_tokens: u32) -> GatewayRequest {
    GatewayRequest::parse(
        &serde_json::to_vec(&json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hello"}],
            "max_completion_tokens": max_completion_tokens
        }))
        .expect("serialize request"),
    )
    .expect("valid request")
}

async fn mount_healthy(server: &MockServer, expected_calls: u64) {
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .expect(expected_calls)
        .mount(server)
        .await;
}

async fn mount_idle_slot(server: &MockServer, expected_calls: u64) {
    Mock::given(method("GET"))
        .and(path("/slots"))
        .and(query_param("fail_on_no_slot", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([{"id": 0, "is_processing": false}])),
        )
        .expect(expected_calls)
        .mount(server)
        .await;
}

#[tokio::test]
async fn stale_health_and_slot_probes_run_concurrently() {
    let server = MockServer::start().await;
    let delay = Duration::from_millis(400);
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(delay)
                .set_body_json(json!({"status": "ok"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/slots"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(delay)
                .set_body_json(json!([{"id": 0, "is_processing": false}])),
        )
        .expect(1)
        .mount(&server)
        .await;
    let config = config(&server, "");
    let admission = LlamaCppAdmission::new(config.local()).expect("adapter");

    let started = Instant::now();
    let state = admission.readiness_state().await;

    assert_eq!(state, LocalAdmissionState::Ready);
    assert!(
        started.elapsed() < Duration::from_millis(650),
        "independent probes should overlap instead of waiting serially"
    );
}

async fn mount_token_count(
    server: &MockServer,
    input_tokens: u32,
    output_tokens: u32,
    expected_calls: u64,
) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .and(body_json(json!({
            "model": "example-local-model",
            "messages": [{"role": "user", "content": "hello"}],
            "max_completion_tokens": output_tokens
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"input_tokens": input_tokens})),
        )
        .expect(expected_calls)
        .mount(server)
        .await;
}

#[tokio::test]
async fn admits_healthy_idle_request_with_exact_context_accounting() {
    let server = MockServer::start().await;
    mount_healthy(&server, 1).await;
    mount_idle_slot(&server, 1).await;
    mount_token_count(&server, 123, 1000, 1).await;
    let config = config(&server, "");
    let admission = LlamaCppAdmission::new(config.local()).expect("adapter");

    let outcome = admission
        .try_admit(&request(1000))
        .await
        .expect("valid request");
    let AdmissionOutcome::Admitted(_lease) = outcome else {
        panic!("request should be admitted");
    };
}

#[tokio::test]
async fn local_semaphore_is_nonblocking_and_released_when_lease_drops() {
    let server = MockServer::start().await;
    mount_healthy(&server, 1).await;
    mount_idle_slot(&server, 2).await;
    mount_token_count(&server, 10, 100, 2).await;
    let config = config(&server, "");
    let admission = LlamaCppAdmission::new(config.local()).expect("adapter");

    let first = admission
        .try_admit(&request(100))
        .await
        .expect("first admission");
    assert_eq!(first.state(), LocalAdmissionState::Ready);

    let second = admission
        .try_admit(&request(100))
        .await
        .expect("busy state");
    assert_eq!(second.state(), LocalAdmissionState::Busy);

    drop(first);
    let third = admission
        .try_admit(&request(100))
        .await
        .expect("permit should be released");
    assert_eq!(third.state(), LocalAdmissionState::Ready);
}

#[tokio::test]
async fn loading_or_malformed_health_is_fail_closed_and_cached() {
    for response in [
        ResponseTemplate::new(200).set_body_json(json!({"status": "loading model"})),
        ResponseTemplate::new(200).set_body_string("not-json"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let config = config(&server, "");
        let admission = LlamaCppAdmission::new(config.local()).expect("adapter");

        for _ in 0..2 {
            let outcome = admission
                .try_admit(&request(100))
                .await
                .expect("admission state");
            assert_eq!(outcome.state(), LocalAdmissionState::Unhealthy);
        }
    }
}

#[tokio::test]
async fn busy_slot_response_does_not_admit_locally() {
    for response in [
        ResponseTemplate::new(503),
        ResponseTemplate::new(200).set_body_json(json!([{"is_processing": true}])),
    ] {
        let server = MockServer::start().await;
        mount_healthy(&server, 1).await;
        Mock::given(method("GET"))
            .and(path("/slots"))
            .and(query_param("fail_on_no_slot", "1"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
        let config = config(&server, "");
        let admission = LlamaCppAdmission::new(config.local()).expect("adapter");

        let outcome = admission
            .try_admit(&request(100))
            .await
            .expect("busy state");
        assert_eq!(outcome.state(), LocalAdmissionState::Busy);
    }
}

#[tokio::test]
async fn rejects_context_overflow_including_output_and_safety_budgets() {
    let server = MockServer::start().await;
    mount_healthy(&server, 1).await;
    mount_idle_slot(&server, 1).await;
    mount_token_count(&server, 64_000, 1000, 1).await;
    let config = config(&server, "");
    let admission = LlamaCppAdmission::new(config.local()).expect("adapter");

    let outcome = admission
        .try_admit(&request(1000))
        .await
        .expect("overflow state");
    assert_eq!(outcome.state(), LocalAdmissionState::ContextOverflow);
}

#[tokio::test]
async fn probe_timeout_is_fail_closed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "ok"}))
                .set_delay(Duration::from_millis(100)),
        )
        .mount(&server)
        .await;
    let config = config(&server, "probe_timeout_ms = 10");
    let admission = LlamaCppAdmission::new(config.local()).expect("adapter");

    let outcome = admission
        .try_admit(&request(100))
        .await
        .expect("timeout state");
    assert_eq!(outcome.state(), LocalAdmissionState::Unhealthy);
}

#[tokio::test]
async fn malformed_token_count_is_fail_closed() {
    let server = MockServer::start().await;
    mount_healthy(&server, 1).await;
    mount_idle_slot(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions/input_tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tokens": []})))
        .expect(1)
        .mount(&server)
        .await;
    let config = config(&server, "");
    let admission = LlamaCppAdmission::new(config.local()).expect("adapter");

    let outcome = admission
        .try_admit(&request(100))
        .await
        .expect("fail-closed state");
    assert_eq!(outcome.state(), LocalAdmissionState::Unhealthy);
}

#[tokio::test]
async fn cancellation_releases_the_local_permit() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"status": "ok"}))
                .set_delay(Duration::from_millis(100)),
        )
        .expect(2)
        .mount(&server)
        .await;
    mount_idle_slot(&server, 2).await;
    mount_token_count(&server, 10, 100, 1).await;
    let config = config(&server, "probe_timeout_ms = 1000");
    let admission = LlamaCppAdmission::new(config.local()).expect("adapter");

    let interrupted_admission = admission.clone();
    let task = tokio::spawn(async move {
        interrupted_admission
            .try_admit(&request(100))
            .await
            .expect("admission")
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    task.abort();
    assert!(
        task.await
            .expect_err("task must be cancelled")
            .is_cancelled()
    );

    let outcome = admission
        .try_admit(&request(100))
        .await
        .expect("permit should be released");
    assert_eq!(outcome.state(), LocalAdmissionState::Ready);
}

#[tokio::test]
async fn panic_unwinding_releases_an_admitted_lease() {
    let server = MockServer::start().await;
    mount_healthy(&server, 1).await;
    mount_idle_slot(&server, 2).await;
    mount_token_count(&server, 10, 100, 2).await;
    let config = config(&server, "");
    let admission = LlamaCppAdmission::new(config.local()).expect("adapter");

    let panicking_admission = admission.clone();
    let task = tokio::spawn(async move {
        let outcome = panicking_admission
            .try_admit(&request(100))
            .await
            .expect("admitted");
        assert_eq!(outcome.state(), LocalAdmissionState::Ready);
        panic!("synthetic handler panic");
    });
    assert!(task.await.expect_err("task must panic").is_panic());

    let outcome = admission
        .try_admit(&request(100))
        .await
        .expect("permit should be released");
    assert_eq!(outcome.state(), LocalAdmissionState::Ready);
}

#[tokio::test]
async fn readiness_reports_current_octoroute_and_llama_capacity() {
    let server = MockServer::start().await;
    mount_healthy(&server, 1).await;
    mount_idle_slot(&server, 1).await;
    let config = config(&server, "");
    let admission = LlamaCppAdmission::new(config.local()).expect("adapter");

    assert_eq!(
        admission.readiness_state().await,
        LocalAdmissionState::Ready
    );
}
