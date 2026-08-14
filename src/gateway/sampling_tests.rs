use super::{
    sampling::DeterministicSampler,
    service_tests::{
        FakeResult, FakeTransport, authorized_headers, body, mount_intelligent_forecast,
        mount_local_admission, service,
    },
    test_support::gateway_config,
};
use axum::http::StatusCode;
use wiremock::MockServer;

#[test]
fn deterministic_sampler_honors_closed_rate_boundaries() {
    assert!(!DeterministicSampler::new(0.0).includes("stable-request"));
    assert!(DeterministicSampler::new(1.0).includes("stable-request"));

    let sampler = DeterministicSampler::new(0.5);
    assert!(sampler.includes("request-c"));
    assert!(!sampler.includes("request-a"));
}

#[tokio::test]
async fn zero_rate_skips_shadow_forecast_and_records_bounded_outcome() {
    let local = MockServer::start().await;
    mount_local_admission(&local).await;
    let config = gateway_config(&local.uri(), "", "", "shadow_sample_rate = 0.0");
    let transport = FakeTransport::default()
        .with_local(FakeResult::Response(StatusCode::OK, r#"{"choices":[]}"#));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(&authorized_headers(), body("auto"))
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "local");
    assert_eq!(transport.local_calls(), 1);
    assert_eq!(transport.cloud_calls(), 0);
    assert!(
        gateway
            .metrics_text()
            .expect("metrics")
            .contains("octoroute_semantic_sampling_total{outcome=\"skipped\"} 1")
    );
}

#[tokio::test]
async fn enforced_forecasts_ignore_shadow_sample_rate() {
    let local = MockServer::start().await;
    mount_intelligent_forecast(&local, 0.1, "unsupported", "known_local_limit", 1).await;
    let config = gateway_config(
        &local.uri(),
        "",
        "",
        "semantic_mode = \"enforced\"\nshadow_sample_rate = 0.0",
    );
    let transport = FakeTransport::default()
        .with_cloud(FakeResult::Response(StatusCode::OK, r#"{"choices":[]}"#));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(&authorized_headers(), body("auto"))
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "cloud");
    assert_eq!(transport.cloud_calls(), 1);
    assert!(
        !gateway
            .metrics_text()
            .expect("metrics")
            .contains("octoroute_semantic_sampling_total")
    );
}
