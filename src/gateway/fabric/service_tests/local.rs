//! Local-pool routing, privacy, and model-listing tests.

use super::*;

#[tokio::test]
async fn v3_worker_route_streams_through_shared_precommit_transport() {
    let server = MockServer::start().await;
    mount_ready_local(&server).await;
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("KIMI_API_KEY", "unused-kimi-key")
        .with("ZAI_API_KEY", "unused-zai-key")
        .with("OPENROUTER_API_KEY", "unused-openrouter-key");
    let environment_audit = environment.clone();
    let service =
        FabricGatewayService::from_config(local_config(&server), environment).expect("service");
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "worker",
            "messages": [{"role": "user", "content": "implement the bounded task"}],
            "stream": true,
            "max_completion_tokens": 1024,
            "reasoning_effort": "medium"
        }))
        .expect("JSON"),
    );

    let response = service.handle_chat(&headers(), body).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-destination")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-pool")
            .and_then(|value| value.to_str().ok()),
        Some("workers")
    );
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-member")
            .and_then(|value| value.to_str().ok()),
        Some("worker-0")
    );
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("stream body");
    assert!(
        std::str::from_utf8(&body)
            .expect("UTF-8")
            .contains("[DONE]")
    );
    let reads = environment_audit.reads();
    assert_eq!(reads, vec!["OCTOROUTE_API_KEY"]);
}

#[tokio::test]
async fn local_only_failure_never_resolves_or_contacts_a_provider() {
    let server = MockServer::start().await;
    let mut config = local_config(&server);
    for pool in config.local_pools.values_mut() {
        pool.enabled = false;
    }
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("KIMI_API_KEY", "unused-kimi-key")
        .with("ZAI_API_KEY", "unused-zai-key")
        .with("OPENROUTER_API_KEY", "unused-openrouter-key");
    let environment_audit = environment.clone();
    let service = FabricGatewayService::from_config(config, environment).expect("service");
    let mut request_headers = headers();
    request_headers.insert(
        "x-octoroute-privacy",
        HeaderValue::from_static("local-only"),
    );
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "keep this local"}]
        }))
        .expect("JSON"),
    );

    let response = service.handle_chat(&request_headers, body).await;
    assert_eq!(response.status(), 503);
    assert_eq!(environment_audit.reads(), vec!["OCTOROUTE_API_KEY"]);
}

#[tokio::test]
async fn v3_models_include_auto_and_all_virtual_routes() {
    let server = MockServer::start().await;
    let service = FabricGatewayService::from_config(
        local_config(&server),
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");
    let models = service.model_ids();
    assert!(models.contains(&"auto".to_string()));
    for model in ["auto-route", "worker", "supervisor", "local", "cloud-sota"] {
        assert!(models.contains(&model.to_string()), "missing {model}");
    }
}
