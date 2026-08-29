//! Local-worker credential boundary tests.

use super::*;

#[tokio::test]
async fn local_credential_rejection_falls_forward_when_the_route_opts_in() {
    let server = MockServer::start().await;
    mount_local_admission(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "local credential rejected"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer openrouter-test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"id": "fallback", "choices": []})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut config = local_config(&server);
    let endpoint = Url::parse(&format!("{}/", server.uri())).expect("mock provider URL");
    set_provider_endpoint(&mut config, "openrouter", endpoint);
    let route = config.routes.get_mut("auto-route").expect("auto route");
    route.steps = vec![
        RouteTarget::LocalPool("workers".to_string()),
        RouteTarget::Provider("openrouter".to_string()),
    ];
    route.fallback_on.insert(FallbackTrigger::Unauthenticated);
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("OPENROUTER_API_KEY", "openrouter-test-key"),
    )
    .expect("service");

    let response = service
        .handle_chat(&headers(), local_request("auto-route"))
        .await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-provider")
            .and_then(|value| value.to_str().ok()),
        Some("openrouter")
    );
    let metrics = service.metrics_text();
    assert!(
        metrics.contains(
            "octoroute_fabric_pool_fallbacks_total{pool=\"workers\",trigger=\"unauthenticated\"} 1"
        ),
        "{metrics}"
    );
}

/// The worker refused Octoroute's credential, not the caller's gateway key.
/// Its body and authentication status must never be forwarded as the caller's.
#[tokio::test]
async fn local_credential_rejection_is_never_the_clients_own_authentication_error() {
    for status in [401, 403, 407] {
        let server = MockServer::start().await;
        mount_local_admission(&server).await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(status).set_body_json(
                json!({"error": {"message": "worker says the gateway key is invalid"}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let service = FabricGatewayService::from_config(
            local_config(&server),
            TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
        )
        .expect("service");

        let response = service
            .handle_chat(&headers(), local_request("worker"))
            .await;

        assert_eq!(response.status(), 502, "upstream status {status}");
        let bytes = response_body(response).await;
        let text = std::str::from_utf8(&bytes).expect("UTF-8");
        let body: Value = serde_json::from_slice(&bytes).expect("error JSON");
        assert_eq!(body["error"]["code"], "local_credential_rejected");
        assert!(
            !text.contains("worker says the gateway key is invalid"),
            "{text}"
        );
    }
}
