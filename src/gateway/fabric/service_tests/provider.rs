//! HTTP provider dispatch, fallback, and permit tests.

use super::*;

#[tokio::test]
async fn open_ai_provider_rewrites_only_destination_and_supplies_bounded_headers() {
    let server = MockServer::start().await;
    let expected_request = json!({
        "model": "glm-5.3",
        "messages": [{"role": "user", "content": "review the architecture"}],
        "stream": true,
        "reasoning_effort": "high",
        "temperature": 0.7,
        "future_field": {"preserved": true}
    });
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer zai-test-key"))
        .and(body_json(expected_request))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-request-id", "provider-request-id")
                .set_body_raw(
                    "data: {\"id\":\"cloud\",\"model\":\"glm-5.3\"}\n\ndata: [DONE]\n\n",
                    "text/event-stream",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;

    let service = FabricGatewayService::from_config(
        single_provider_config(&server, "zai"),
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("ZAI_API_KEY", "zai-test-key"),
    )
    .expect("service");
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "review the architecture"}],
            "stream": true,
            "reasoning_effort": "high",
            "temperature": 0.7,
            "future_field": {"preserved": true}
        }))
        .expect("JSON"),
    );

    let response = service.handle_chat(&headers(), body).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-provider")
            .and_then(|value| value.to_str().ok()),
        Some("zai")
    );
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-destination")
            .and_then(|value| value.to_str().ok()),
        Some("cloud")
    );
    // The upstream sent its own `x-request-id`; the gateway's must win, and the
    // two correlation headers must agree.
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .expect("gateway request id")
        .to_string();
    assert_ne!(request_id, "provider-request-id");
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-request-id")
            .and_then(|value| value.to_str().ok()),
        Some(request_id.as_str())
    );
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("stream body");
    assert!(
        std::str::from_utf8(&body)
            .expect("UTF-8")
            .contains("[DONE]")
    );
    let received = server.received_requests().await.expect("request recording");
    let upstream = received.last().expect("OpenAI dispatch request");
    let authorization: Vec<&str> = upstream
        .headers
        .get_all("authorization")
        .iter()
        .map(|value| value.to_str().expect("ASCII header value"))
        .collect();
    assert_eq!(
        authorization,
        vec!["Bearer zai-test-key"],
        "the OpenAI provider must receive only its own credential"
    );
}

/// A missing credential must surface rather than silently rerouting the traffic,
/// and the spend it carries, to the next provider. The prompt still reaches no
/// provider whose credential could not be resolved.
#[tokio::test]
async fn missing_provider_credential_surfaces_instead_of_rerouting_spend() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"id": "cloud", "model": "openrouter/auto", "choices": []})),
        )
        .expect(0)
        .mount(&server)
        .await;

    let mut config = local_config(&server);
    let endpoint = Url::parse(&format!("{}/", server.uri())).expect("mock provider URL");
    set_provider_endpoint(&mut config, "zai", endpoint.clone());
    set_provider_endpoint(&mut config, "openrouter", endpoint);
    config
        .routes
        .get_mut("cloud-sota")
        .expect("cloud route")
        .steps = vec![
        RouteTarget::Provider("zai".to_string()),
        RouteTarget::Provider("openrouter".to_string()),
    ];
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("OPENROUTER_API_KEY", "openrouter-test-key");
    let environment_audit = environment.clone();
    let service = FabricGatewayService::from_config(config, environment).expect("service");

    let response = service.handle_chat(&headers(), cloud_request()).await;
    assert_eq!(response.status(), 503);
    let body = response_body(response).await;
    let body: Value = serde_json::from_slice(&body).expect("error JSON");
    assert_eq!(body["error"]["code"], "provider_unauthenticated");
    // The second provider was never dispatched, so no prompt was disclosed to it.
    assert_eq!(
        environment_audit.reads(),
        vec!["OCTOROUTE_API_KEY", "ZAI_API_KEY"]
    );
    let metrics = service.metrics_text();
    assert!(metrics.contains(
        "octoroute_fabric_provider_admissions_total{provider=\"zai\",state=\"unauthenticated\"} 1"
    ));
}

/// An operator can still opt into credential fall-forward per route, which is
/// what keeps the strict default a policy choice rather than a hard limit.
#[tokio::test]
async fn unauthenticated_fallback_is_available_when_explicitly_configured() {
    let server = MockServer::start().await;
    let expected_request = json!({
        "model": "openrouter/auto",
        "messages": [{"role": "user", "content": "review the architecture"}],
        "temperature": 0.2,
        "plugins": [
            {"id": "preserved-plugin", "setting": true},
            {"id": "auto-router", "cost_quality_tradeoff": 9}
        ]
    });
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer openrouter-test-key"))
        .and(header("x-openrouter-title", "Octoroute"))
        .and(body_json(expected_request))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"id": "cloud", "model": "openrouter/auto", "choices": []})),
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
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("OPENROUTER_API_KEY", "openrouter-test-key");
    let environment_audit = environment.clone();
    let service = FabricGatewayService::from_config(config, environment).expect("service");

    let response = service.handle_chat(&headers(), cloud_request()).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("x-octoroute-provider")
            .and_then(|value| value.to_str().ok()),
        Some("openrouter")
    );
    assert_eq!(
        environment_audit.reads(),
        vec!["OCTOROUTE_API_KEY", "ZAI_API_KEY", "OPENROUTER_API_KEY"]
    );
    let metrics = service.metrics_text();
    assert!(metrics.contains(
        "octoroute_fabric_provider_fallbacks_total{provider=\"zai\",trigger=\"unauthenticated\"} 1"
    ));
    assert!(metrics.contains(
        "octoroute_fabric_provider_responses_total{provider=\"openrouter\",outcome=\"success\"} 1"
    ));
}

#[tokio::test]
async fn provider_response_fallback_obeys_the_closed_trigger_set() {
    for (first_status, remove_trigger, expected_status, expected_provider, falls_forward) in [
        (429, None, 200, Some("openrouter"), true),
        (
            429,
            Some(FallbackTrigger::RateLimited),
            429,
            Some("zai"),
            false,
        ),
        (503, None, 200, Some("openrouter"), true),
        // `unauthenticated` is outside the default set, so this route commits.
        // The upstream's own 401 is not passed through: to the client it would
        // read as its own gateway credential failing, so the credential
        // rejection is reported as an upstream fault instead.
        (401, None, 502, None, false),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer zai-test-key"))
            .respond_with(
                ResponseTemplate::new(first_status)
                    .insert_header("content-type", "application/json")
                    .set_body_json(json!({"error": {"message": "bounded fixture"}})),
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
            .expect(if falls_forward { 1 } else { 0 })
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
        if let Some(trigger) = remove_trigger {
            route.fallback_on.remove(&trigger);
        }
        let service = FabricGatewayService::from_config(
            config,
            TestEnvironment::default()
                .with("OCTOROUTE_API_KEY", "inbound-test-key")
                .with("ZAI_API_KEY", "zai-test-key")
                .with("OPENROUTER_API_KEY", "openrouter-test-key"),
        )
        .expect("service");
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "cloud-sota",
                "messages": [{"role": "user", "content": "review the boundary"}]
            }))
            .expect("JSON"),
        );

        let response = service.handle_chat(&headers(), body).await;
        assert_eq!(response.status().as_u16(), expected_status);
        assert_eq!(
            response
                .headers()
                .get("x-octoroute-provider")
                .and_then(|value| value.to_str().ok()),
            expected_provider
        );
        drop(response);
    }
}

#[tokio::test]
async fn provider_permit_is_held_until_the_streaming_body_is_dropped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer zai-test-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"id": "cloud", "model": "glm-5.3", "choices": []})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut config = single_provider_config(&server, "zai");
    config
        .providers
        .get_mut("zai")
        .expect("zai provider")
        .max_in_flight = 1;
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("ZAI_API_KEY", "zai-test-key"),
    )
    .expect("service");
    let request = || {
        Bytes::from(
            serde_json::to_vec(&json!({
                "model": "cloud-sota",
                "messages": [{"role": "user", "content": "review the architecture"}]
            }))
            .expect("JSON"),
        )
    };

    let held = service.handle_chat(&headers(), request()).await;
    assert_eq!(held.status(), 200);
    let busy = service.handle_chat(&headers(), request()).await;
    assert_eq!(busy.status(), 503);
    let body = to_bytes(busy.into_body(), 1024 * 1024)
        .await
        .expect("error body");
    assert!(
        std::str::from_utf8(&body)
            .expect("UTF-8")
            .contains("provider_busy")
    );
    drop(held);
}

#[tokio::test]
async fn provider_readiness_probes_auth_once_per_cache_window() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", "Bearer zai-test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
        .expect(1)
        .mount(&server)
        .await;

    let config = single_enabled_provider_config(&server, "zai");
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("ZAI_API_KEY", "zai-test-key");
    let environment_audit = environment.clone();
    let service = FabricGatewayService::from_config(config, environment).expect("service");

    for _ in 0..2 {
        let readiness = service.readiness().await;
        assert_eq!(
            readiness.providers().get("zai"),
            Some(&ProviderAdmissionState::Ready)
        );
    }
    assert_eq!(
        environment_audit.reads(),
        vec!["OCTOROUTE_API_KEY", "ZAI_API_KEY"]
    );
    assert!(
        service
            .metrics_text()
            .contains("octoroute_fabric_provider_probes_total{provider=\"zai\",state=\"ready\"} 1")
    );
}

#[tokio::test]
async fn anthropic_tools_preserve_the_requested_response_format() {
    for (requested_stream, stream) in [(None, false), (Some(false), false), (Some(true), true)] {
        let server = MockServer::start().await;
        let expected_request = json!({
            "model": "k3",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "inspect the repository"}]
            }],
            "max_tokens": 200000,
            "stream": stream,
            "thinking": {"type": "enabled", "budget_tokens": 16384},
            "tools": [{
                "name": "read_file",
                "description": "Read one repository file",
                "input_schema": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }
            }],
            "tool_choice": {"type": "auto"}
        });
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-kimi\",\"model\":\"k3\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call-1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"src/main.rs\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":12}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let upstream_response = if stream {
            ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream")
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "type": "message",
                "id": "msg-kimi",
                "model": "k3",
                "content": [{
                    "type": "tool_use",
                    "id": "call-1",
                    "name": "read_file",
                    "input": {"path": "src/main.rs"}
                }],
                "stop_reason": "tool_use"
            }))
        };
        Mock::given(method("POST"))
            .and(path("/messages"))
            .and(header("x-api-key", "kimi-test-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .and(body_json(expected_request))
            .respond_with(upstream_response)
            .expect(1)
            .mount(&server)
            .await;

        let service = FabricGatewayService::from_config(
            single_provider_config(&server, "kimi"),
            TestEnvironment::default()
                .with("OCTOROUTE_API_KEY", "inbound-test-key")
                .with("KIMI_API_KEY", "kimi-test-key"),
        )
        .expect("service");
        let mut request = json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "inspect the repository"}],
            "reasoning_effort": "high",
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "description": "Read one repository file",
                    "parameters": {
                        "type": "object",
                        "properties": {"path": {"type": "string"}},
                        "required": ["path"]
                    }
                }
            }],
            "tool_choice": "auto"
        });
        if let Some(requested_stream) = requested_stream {
            request["stream"] = json!(requested_stream);
        }
        let body = Bytes::from(serde_json::to_vec(&request).expect("JSON"));
        let response = service.handle_chat(&headers(), body).await;
        assert_eq!(response.status(), 200, "stream={requested_stream:?}");
        assert_eq!(
            response.headers()["content-type"],
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            },
            "stream={requested_stream:?} must determine the response format"
        );
        assert_eq!(
            response
                .headers()
                .get("x-octoroute-provider")
                .and_then(|value| value.to_str().ok()),
            Some("kimi")
        );
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("translated response");
        let body = std::str::from_utf8(&body).expect("UTF-8");
        if stream {
            assert!(body.contains("chat.completion.chunk"), "{body}");
            assert!(body.contains("tool_calls"), "{body}");
            assert!(body.contains("src/main.rs"), "{body}");
            assert!(body.contains("data: [DONE]"), "{body}");
        } else {
            let body: Value = serde_json::from_str(body).expect("buffered OpenAI JSON");
            assert_eq!(body["object"], "chat.completion");
            assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
            assert_eq!(
                body["choices"][0]["message"]["tool_calls"][0],
                json!({
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"src/main.rs\"}"}
                })
            );
        }
        let received = server.received_requests().await.expect("request recording");
        let upstream = received.last().expect("Anthropic dispatch request");
        assert_eq!(
            upstream
                .headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            Some("kimi-test-key"),
            "the Anthropic provider must receive its own credential"
        );
        assert!(
            upstream.headers.get("authorization").is_none(),
            "the inbound bearer must never travel to an Anthropic provider"
        );
    }
}

/// A 5xx from `/models` is an outage and must still report as one, so the 404
/// behavior covered in `service_tests::readiness` is specific to that status
/// rather than to every failure.
#[tokio::test]
async fn a_failing_models_endpoint_still_reports_unavailable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(502))
        .expect(1)
        .mount(&server)
        .await;

    let config = single_enabled_provider_config(&server, "zai");
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default()
            .with("OCTOROUTE_API_KEY", "inbound-test-key")
            .with("ZAI_API_KEY", "zai-test-key"),
    )
    .expect("service");

    assert_eq!(
        service.readiness().await.providers().get("zai"),
        Some(&ProviderAdmissionState::Unavailable)
    );
}
