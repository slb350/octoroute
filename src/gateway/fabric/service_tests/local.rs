//! Local-pool routing, privacy, model listing, and route-executor tests.

use super::*;

mod governing;

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

/// A member that admits and then fails before commitment is still local
/// capacity spilling to the next step. Without the pool fallback counter the
/// spill is attributed to whichever step finally answered.
#[tokio::test]
async fn local_precommit_failure_records_a_pool_fallback() {
    let server = MockServer::start().await;
    mount_local_admission(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"error": "member crashed"})))
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

    // `local` is [pool:workers, pool:supervisor-local], so the failure falls
    // forward onto the disabled supervisor rather than reaching the client.
    assert_eq!(response.status(), 503);
    let metrics = service.metrics_text();
    assert!(
        metrics.contains(
            "octoroute_fabric_pool_fallbacks_total{pool=\"workers\",trigger=\"precommit_failure\"} 1"
        ),
        "{metrics}"
    );
}

/// The provider path honours `rate_limited`; a local member answering 429 means
/// the same thing and must be classified the same way.
#[tokio::test]
async fn local_upstream_rate_limit_uses_the_rate_limited_trigger() {
    let server = MockServer::start().await;
    mount_local_admission(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({"error": "slow down"})))
        .expect(1)
        .mount(&server)
        .await;
    let mut config = local_config(&server);
    config
        .routes
        .get_mut("local")
        .expect("local route")
        .fallback_on
        .insert(FallbackTrigger::RateLimited);
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    let response = service
        .handle_chat(&headers(), local_request("local"))
        .await;

    // The 429 was a fall-forward trigger, not a response: the terminal answer
    // comes from the disabled supervisor step.
    assert_eq!(response.status(), 503);
    let metrics = service.metrics_text();
    assert!(
        metrics.contains(
            "octoroute_fabric_pool_fallbacks_total{pool=\"workers\",trigger=\"rate_limited\"} 1"
        ),
        "{metrics}"
    );
}

/// 503 typed `invalid_request_error` tells a client to retry and to fix its
/// request at the same time.
#[tokio::test]
async fn an_unroutable_privacy_narrowing_reports_an_upstream_error_type() {
    let server = MockServer::start().await;
    let mut config = local_config(&server);
    // A provider-only route that privacy may narrow rather than refuse.
    config
        .routes
        .get_mut("cloud-sota")
        .expect("cloud route")
        .privacy = RoutePrivacy::CloudAllowed;
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");
    let mut request_headers = headers();
    request_headers.insert(
        "x-octoroute-privacy",
        HeaderValue::from_static("local-only"),
    );

    let response = service
        .handle_chat(&request_headers, local_request("cloud-sota"))
        .await;

    assert_eq!(response.status(), 503);
    let body: Value = serde_json::from_slice(&response_body(response).await).expect("error JSON");
    assert_eq!(body["error"]["type"], "upstream_error");
    assert_eq!(body["error"]["code"], "routing_error");
}

/// A provider that refuses Octoroute's credential is the same operator
/// condition as one whose credential could not be resolved, so a route that
/// opted into `unauthenticated` gets it honoured at dispatch too.
#[tokio::test]
async fn provider_credential_rejection_falls_forward_when_the_route_opts_in() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer stale-zai-key"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "invalid api key"}})),
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
    set_provider_endpoint(&mut config, "zai", endpoint.clone());
    set_provider_endpoint(&mut config, "openrouter", endpoint);
    let route = config.routes.get_mut("cloud-sota").expect("cloud route");
    route.steps = vec![
        RouteTarget::Provider("zai".to_string()),
        RouteTarget::Provider("openrouter".to_string()),
    ];
    route.fallback_on.insert(FallbackTrigger::Unauthenticated);
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("ZAI_API_KEY", "stale-zai-key")
            .with("OPENROUTER_API_KEY", "openrouter-test-key"),
    )
    .expect("service");

    let response = service.handle_chat(&headers(), cloud_request()).await;

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
            "octoroute_fabric_provider_fallbacks_total{provider=\"zai\",trigger=\"unauthenticated\"} 1"
        ),
        "{metrics}"
    );
}

/// Forwarding the upstream's 401 tells the caller that the bearer it sent this
/// gateway was refused, which is a different problem with a different fix.
#[tokio::test]
async fn provider_credential_rejection_is_never_the_clients_own_401() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "provider says the key is invalid"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let service = FabricGatewayService::from_config(
        single_provider_config(&server, "zai"),
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("ZAI_API_KEY", "stale-zai-key"),
    )
    .expect("service");

    let response = service.handle_chat(&headers(), cloud_request()).await;

    assert_eq!(response.status(), 502);
    let bytes = response_body(response).await;
    let text = std::str::from_utf8(&bytes).expect("UTF-8").to_string();
    let body: Value = serde_json::from_slice(&bytes).expect("error JSON");
    assert_eq!(body["error"]["code"], "provider_credential_rejected");
    assert!(!text.contains("provider says the key is invalid"), "{text}");
}

/// The caller's own malformed `plugins` is still the caller's error; only
/// gateway-side translation failures move off `invalid_request_error`.
#[tokio::test]
async fn a_malformed_client_plugins_field_stays_a_client_error() {
    let server = MockServer::start().await;
    let service = FabricGatewayService::from_config(
        single_provider_config(&server, "openrouter"),
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("OPENROUTER_API_KEY", "openrouter-test-key"),
    )
    .expect("service");
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "review the architecture"}],
            "plugins": "not-an-array"
        }))
        .expect("JSON"),
    );

    let response = service.handle_chat(&headers(), body).await;

    assert_eq!(response.status(), 400);
    let body: Value = serde_json::from_slice(&response_body(response).await).expect("error JSON");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "provider_request_invalid");
    // The caller's own error is not a provider admission rejection.
    let metrics = service.metrics_text();
    assert!(
        metrics.contains(
            "octoroute_fabric_provider_admissions_total{provider=\"openrouter\",state=\"incompatible\"} 0"
        ),
        "{metrics}"
    );
}

/// A client that disconnects mid-body never breached the size limit, and
/// answering it with 413 tells an operator to shrink a request that was never
/// finished.
#[tokio::test]
async fn an_incomplete_request_body_is_not_reported_as_too_large() {
    let server = MockServer::start().await;
    let service = FabricGatewayService::from_config(
        local_config(&server),
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");
    let stream = futures::stream::iter(vec![
        Ok::<Bytes, std::io::Error>(Bytes::from_static(b"{\"model\":\"worker\"")),
        Err(std::io::Error::other("client went away")),
    ]);
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(AUTHORIZATION, "Bearer inbound-test-key")
        .body(Body::from_stream(stream))
        .expect("request");

    let response = service.handle_http_chat(request).await;

    assert_eq!(response.status(), 400);
    let body: Value = serde_json::from_slice(&response_body(response).await).expect("error JSON");
    assert_eq!(body["error"]["code"], "request_body_incomplete");
}

/// A body exactly at the limit is within it. Refusing it tells a client to
/// shrink a request the operator's own configuration allows.
#[tokio::test]
async fn a_request_body_exactly_at_the_limit_is_accepted() {
    let server = MockServer::start().await;
    mount_local_admission(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"id": "local", "choices": []})),
        )
        .mount(&server)
        .await;
    let body = local_request("worker");
    let mut config = local_config(&server);
    config.server.max_request_bytes = body.len();
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");
    let request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header(AUTHORIZATION, "Bearer inbound-test-key")
        .body(Body::from(body))
        .expect("request");

    let response = service.handle_http_chat(request).await;

    assert_eq!(response.status(), 200);
}

/// Only a 429 is a rate limit. A route that is merely *capable* of falling
/// forward on one must still commit the local response it was given, or every
/// admitted local answer is thrown away and re-bought from a provider.
#[tokio::test]
async fn an_admitted_local_response_commits_on_a_route_that_could_fall_forward() {
    let server = MockServer::start().await;
    mount_local_admission(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"id": "local", "choices": []})),
        )
        .mount(&server)
        .await;
    let service = FabricGatewayService::from_config(
        local_config(&server),
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    // `auto-route` carries `rate_limited` in `fallback_on` and has four steps
    // after this one, so the fall-forward guard alone is satisfied.
    let response = service
        .handle_chat(&headers(), local_request("auto-route"))
        .await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-destination")
            .and_then(|value| value.to_str().ok()),
        Some("local")
    );
}

/// A provider body the gateway cannot build is the gateway's fault. Answering
/// 400 tells a caller to fix a request that was already valid, and leaving it
/// out of the admission counters hides it from `/metrics` entirely.
#[tokio::test]
async fn a_gateway_side_translation_failure_is_not_the_clients_error() {
    let server = MockServer::start().await;
    let mut config = single_provider_config(&server, "zai");
    // Not representable as JSON, so the configured temperature cannot be
    // written into the outbound body.
    config
        .providers
        .get_mut("zai")
        .expect("zai provider")
        .temperature = Some(f64::NAN);
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("ZAI_API_KEY", "zai-test-key"),
    )
    .expect("service");

    let response = service.handle_chat(&headers(), cloud_request()).await;

    assert_eq!(response.status(), 502);
    let body: Value = serde_json::from_slice(&response_body(response).await).expect("error JSON");
    assert_eq!(body["error"]["type"], "upstream_error");
    assert_eq!(body["error"]["code"], "provider_translation_failed");
    let metrics = service.metrics_text();
    assert!(
        metrics.contains(
            "octoroute_fabric_provider_admissions_total{provider=\"zai\",state=\"incompatible\"} 1"
        ),
        "{metrics}"
    );
}
