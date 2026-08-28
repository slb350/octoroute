//! Redirect refusal: a 3xx from any upstream is a pre-commit failure.

use super::*;

/// A 3xx from a local member is a pre-commit failure, never an answer.
///
/// The gateway's client does not follow redirects, so a 302 means the member
/// endpoint points somewhere the operator did not configure. Committing it
/// would hand the client a redirect it would then follow itself.
#[tokio::test]
async fn a_local_member_redirect_is_a_precommit_failure_and_is_never_followed() {
    let redirect_target = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "hijacked"})))
        .expect(0)
        .mount(&redirect_target)
        .await;
    let server = MockServer::start().await;
    mount_local_admission(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            format!("{}/v1/chat/completions", redirect_target.uri()).as_str(),
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
        .handle_chat(&headers(), local_request("local"))
        .await;

    // `local` is [pool:workers, pool:supervisor-local]: the redirect falls
    // forward onto the disabled supervisor rather than reaching the client.
    assert_eq!(response.status(), 503);
    assert!(
        redirect_target
            .received_requests()
            .await
            .expect("request recording")
            .is_empty(),
        "the redirect target must never be contacted"
    );
    let metrics = service.metrics_text();
    assert!(
        metrics.contains(
            "octoroute_fabric_pool_fallbacks_total{pool=\"workers\",trigger=\"precommit_failure\"} 1"
        ),
        "{metrics}"
    );
}

/// The same refusal on the provider path, over the protocol that makes it a
/// credential leak: Anthropic sends the key in `x-api-key`, which reqwest does
/// not strip across origins.
#[tokio::test]
async fn a_provider_redirect_is_a_precommit_failure_and_is_never_followed() {
    let redirect_target = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "hijacked"})))
        .expect(0)
        .mount(&redirect_target)
        .await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(ResponseTemplate::new(307).insert_header(
            "location",
            format!("{}/messages", redirect_target.uri()).as_str(),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
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
    set_provider_endpoint(&mut config, "kimi", endpoint.clone());
    set_provider_endpoint(&mut config, "openrouter", endpoint);
    config
        .routes
        .get_mut("cloud-sota")
        .expect("cloud route")
        .steps = vec![
        RouteTarget::Provider("kimi".to_string()),
        RouteTarget::Provider("openrouter".to_string()),
    ];
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("KIMI_API_KEY", "kimi-test-key")
            .with("OPENROUTER_API_KEY", "openrouter-test-key"),
    )
    .expect("service");

    let response = service
        .handle_chat(&headers(), portable_cloud_request())
        .await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-provider")
            .and_then(|value| value.to_str().ok()),
        Some("openrouter")
    );
    assert!(
        redirect_target
            .received_requests()
            .await
            .expect("request recording")
            .is_empty(),
        "the redirect target must never receive the provider credential"
    );
    let metrics = service.metrics_text();
    assert!(
        metrics.contains(
            "octoroute_fabric_provider_fallbacks_total{provider=\"kimi\",trigger=\"precommit_failure\"} 1"
        ),
        "{metrics}"
    );
}
