//! Codex CLI provider tests.

use super::*;

#[tokio::test]
async fn incompatible_codex_request_fails_closed_without_launching_the_cli() {
    let server = MockServer::start().await;
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("OPENROUTER_API_KEY", "unused-openrouter-key");
    let environment_audit = environment.clone();
    // Scoped to the Codex step alone: this asserts Codex fails closed, not what
    // the route does afterwards. `incompatible_codex_request_falls_forward_to_a_
    // capable_provider` covers the fall-through.
    let mut config = local_config(&server);
    config
        .routes
        .get_mut("cloud-sota")
        .expect("cloud route")
        .steps = vec![RouteTarget::Provider("codex".to_string())];
    let service = FabricGatewayService::from_config(config, environment).expect("service");
    let incompatible_requests = [
        json!({
            "model": "cloud-sota",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image_url",
                    "image_url": {"url": "data:image/png;base64,AA=="}
                }]
            }]
        }),
        json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "two answers"}],
            "n": 2
        }),
    ];

    for request in incompatible_requests {
        let body = Bytes::from(serde_json::to_vec(&request).expect("JSON"));
        let response = service.handle_chat(&headers(), body).await;
        assert_eq!(response.status(), 503);
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("error body");
        assert!(
            std::str::from_utf8(&body)
                .expect("UTF-8")
                .contains("provider_incompatible")
        );
    }
    assert_eq!(environment_audit.reads(), vec!["OCTOROUTE_API_KEY"]);
}

#[tokio::test]
async fn codex_cli_dispatch_is_ephemeral_filtered_and_open_ai_compatible() {
    use std::os::unix::fs::PermissionsExt as _;

    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary Codex fixture");
    let executable = directory.path().join("fake-codex");
    std::fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "if [ \"${1:-}\" = doctor ]; then\n",
            "  printf '%s' '{\"schemaVersion\":1,\"codexVersion\":\"0.148.0\",\"checks\":{\"auth.credentials\":{\"details\":{\"stored ChatGPT tokens\":\"true\",\"stored auth mode\":\"chatgpt\"}}}}'\n",
            "  exit 0\n",
            "fi\n",
            "sed -n '1,$p' >/dev/null\n",
            "printf '%s\\n' \\\n",
            "  '{\"type\":\"thread.started\",\"thread_id\":\"redacted\"}' \\\n",
            "  '{\"type\":\"turn.started\"}' \\\n",
            "  '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"{\\\"content\\\":\\\"Codex answer\\\",\\\"reasoning_content\\\":null,\\\"tool_calls\\\":[],\\\"finish_reason\\\":\\\"stop\\\"}\"}}' \\\n",
            "  '{\"type\":\"turn.completed\"}'\n"
        ),
    )
    .expect("fake Codex executable");
    let mut permissions = std::fs::metadata(&executable)
        .expect("fake Codex metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).expect("fake Codex permissions");

    let mut config = local_config(&server);
    for provider in config.providers.values_mut() {
        provider.enabled = false;
    }
    let codex = config.providers.get_mut("codex").expect("codex provider");
    codex.enabled = true;
    codex.runtime = ProviderRuntimeConfig::CodexCli {
        executable: executable.to_string_lossy().into_owned(),
    };
    config
        .routes
        .get_mut("cloud-sota")
        .expect("cloud route")
        .steps = vec![RouteTarget::Provider("codex".to_string())];
    let service = FabricGatewayService::from_config(
        config,
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");
    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "review this change"}]
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
        Some("codex")
    );
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("Codex response");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("OpenAI JSON response");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "Codex answer");

    let readiness = service.readiness().await;
    assert_eq!(
        readiness.providers().get("codex"),
        Some(&ProviderAdmissionState::Ready)
    );
}

/// Codex cannot serve image, audio, or video requests. The shipped `cloud-sota`
/// route must fall through to a provider that can, rather than answering 503
/// while a capable provider sits behind it in the same route.
#[tokio::test]
async fn incompatible_codex_request_falls_forward_to_a_capable_provider() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
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
    set_provider_endpoint(&mut config, "openrouter", endpoint);
    config
        .routes
        .get_mut("cloud-sota")
        .expect("cloud route")
        .steps = vec![
        RouteTarget::Provider("codex".to_string()),
        RouteTarget::Provider("openrouter".to_string()),
    ];
    let environment = TestEnvironment::default()
        .with("OCTOROUTE_API_KEY", "inbound-test-key")
        .with("OPENROUTER_API_KEY", "openrouter-test-key");
    let service = FabricGatewayService::from_config(config, environment).expect("service");

    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "image_url",
                    "image_url": {"url": "data:image/png;base64,AA=="}
                }]
            }]
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
        Some("openrouter")
    );
}

/// The shipped `cloud-sota` route carries `incompatible`, so a request its first
/// step cannot serve reaches a step that can.
#[test]
fn shipped_cloud_route_falls_forward_on_an_incompatible_first_step() {
    let config = FabricConfig::from_toml(include_str!("../../../../config.toml")).expect("config");
    let route = &config.routes["cloud-sota"];
    assert_eq!(
        route.steps.first(),
        Some(&RouteTarget::Provider("codex".to_string()))
    );
    assert!(route.fallback_on.contains(&FallbackTrigger::Incompatible));
}
