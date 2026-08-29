//! Codex CLI provider tests.

use super::*;

#[cfg(unix)]
use crate::gateway::fabric::test_support::write_executable_fixture;

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

/// A `codex doctor --json` diagnostic with the requested stored auth mode.
#[cfg(unix)]
fn diagnostic_json(auth_mode: &str, chatgpt_tokens: &str) -> String {
    format!(
        "{{\"schemaVersion\":1,\"codexVersion\":\"0.148.0\",\"checks\":{{\"auth.credentials\":{{\"details\":{{\"stored ChatGPT tokens\":\"{chatgpt_tokens}\",\"stored auth mode\":\"{auth_mode}\"}}}}}}}}"
    )
}

/// A `codex doctor` payload with the requested stored auth mode.
#[cfg(unix)]
fn doctor_branch(auth_mode: &str, chatgpt_tokens: &str) -> String {
    format!(
        concat!(
            "#!/bin/sh\n",
            "if [ \"${{1:-}}\" = doctor ]; then\n",
            "  printf '%s' '{diagnostic}'\n",
            "  exit 0\n",
            "fi\n"
        ),
        diagnostic = diagnostic_json(auth_mode, chatgpt_tokens)
    )
}

/// Point the `codex` provider at a fake CLI and make it the only cloud step.
#[cfg(unix)]
fn codex_only_config(server: &MockServer, executable: &std::path::Path) -> FabricConfig {
    let mut config = local_config(server);
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
    config
}

#[cfg(unix)]
#[tokio::test]
async fn codex_cli_dispatch_is_ephemeral_filtered_and_open_ai_compatible() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary Codex fixture");
    let executable = write_executable_fixture(
        directory.path(),
        "fake-codex",
        &format!(
            "{}{}",
            doctor_branch("chatgpt", "true"),
            concat!(
                "sed -n '1,$p' >/dev/null\n",
                "printf '%s\\n' \\\n",
                "  '{\"type\":\"thread.started\",\"thread_id\":\"redacted\"}' \\\n",
                "  '{\"type\":\"turn.started\"}' \\\n",
                "  '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"{\\\"content\\\":\\\"Codex answer\\\",\\\"reasoning_content\\\":null,\\\"tool_calls\\\":[],\\\"finish_reason\\\":\\\"stop\\\"}\"}}' \\\n",
                "  '{\"type\":\"turn.completed\"}'\n"
            )
        ),
    );

    let service = FabricGatewayService::from_config(
        codex_only_config(&server, &executable),
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

/// A Codex CLI logged in with an API key instead of a ChatGPT subscription is
/// an operator error, not an outage. Reporting it as `Unavailable` maps it to
/// the `unhealthy` trigger, which is in the default fallback set, so every
/// request and its spend would spill silently to the next step.
#[cfg(unix)]
#[tokio::test]
async fn api_key_codex_auth_reports_unauthenticated_rather_than_unavailable() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary Codex fixture");
    let executable = write_executable_fixture(
        directory.path(),
        "fake-codex",
        &format!("{}exit 0\n", doctor_branch("api", "false")),
    );
    let service = FabricGatewayService::from_config(
        codex_only_config(&server, &executable),
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    let readiness = service.readiness().await;
    assert_eq!(
        readiness.providers().get("codex"),
        Some(&ProviderAdmissionState::Unauthenticated)
    );
}

/// A doctor payload that is not the contract Octoroute checks is the same class
/// of misconfiguration, and must not read as a transient outage either.
#[cfg(unix)]
#[tokio::test]
async fn an_unparseable_codex_diagnostic_reports_unauthenticated() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary Codex fixture");
    let executable = write_executable_fixture(
        directory.path(),
        "fake-codex",
        "#!/bin/sh\nif [ \"${1:-}\" = doctor ]; then\n  printf '%s' 'not json'\n  exit 0\nfi\nexit 0\n",
    );
    let service = FabricGatewayService::from_config(
        codex_only_config(&server, &executable),
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    assert_eq!(
        service.readiness().await.providers().get("codex"),
        Some(&ProviderAdmissionState::Unauthenticated)
    );
}

/// A CLI that never answers must not hold the provider and inbound permits for
/// the whole 30-minute total timeout: `first_byte_timeout_ms` is configurable
/// for a `codex_cli` provider and has to actually bound the run.
#[cfg(unix)]
#[tokio::test]
async fn a_hung_codex_run_is_bounded_by_the_first_byte_deadline() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().expect("temporary Codex fixture");
    let executable = write_executable_fixture(
        directory.path(),
        "fake-codex",
        &format!("{}sleep 30\n", doctor_branch("chatgpt", "true")),
    );
    let mut config = codex_only_config(&server, &executable);
    let codex = config.providers.get_mut("codex").expect("codex provider");
    codex.first_byte_timeout_ms = Some(250);
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

    let started = std::time::Instant::now();
    let response = service.handle_chat(&headers(), body).await;
    let elapsed = started.elapsed();
    assert_ne!(response.status(), 200);
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "the deadline must cut the run short; took {elapsed:?}"
    );
}

/// A `codex doctor` that answers differently the second time it is asked.
///
/// The first probe reports a ChatGPT subscription; every later one reports an
/// API-key login. That makes a second probe visible in the readiness verdict
/// itself, with no counter to read back, and the `exec` branch fails so a
/// dispatch through this CLI is a dispatch failure.
#[cfg(unix)]
fn doctor_that_changes_its_answer(marker: &std::path::Path) -> String {
    format!(
        concat!(
            "#!/bin/sh\n",
            "if [ \"${{1:-}}\" = doctor ]; then\n",
            "  if [ -e {marker} ]; then\n",
            "    printf '%s' '{api}'\n",
            "  else\n",
            "    : > {marker}\n",
            "    printf '%s' '{chatgpt}'\n",
            "  fi\n",
            "  exit 0\n",
            "fi\n",
            "exit 7\n"
        ),
        marker = marker.display(),
        api = diagnostic_json("api", "false"),
        chatgpt = diagnostic_json("chatgpt", "true")
    )
}

/// A dispatch that fails has to discard the cached readiness verdict it just
/// contradicted.
///
/// Readiness is otherwise re-probed only on its TTL, which defaults to thirty
/// seconds and is configurable up to an hour: without invalidation a Codex CLI
/// that lost its subscription mid-window keeps reporting `ready` on `/health`
/// for the rest of it, while every request through it fails.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_codex_dispatch_discards_the_cached_readiness_verdict() {
    let directory = tempfile::tempdir().expect("temporary Codex fixture");
    let server = MockServer::start().await;
    let executable = write_executable_fixture(
        directory.path(),
        "fake-codex",
        &doctor_that_changes_its_answer(&directory.path().join("probed")),
    );
    let service = FabricGatewayService::from_config(
        codex_only_config(&server, &executable),
        TestEnvironment::default().with("OCTOROUTE_API_KEY", "inbound-test-key"),
    )
    .expect("service");

    assert_eq!(
        service.readiness().await.providers().get("codex"),
        Some(&ProviderAdmissionState::Ready),
        "the first probe caches a ready verdict"
    );

    let body = Bytes::from(
        serde_json::to_vec(&json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "review this change"}]
        }))
        .expect("JSON"),
    );
    assert_ne!(service.handle_chat(&headers(), body).await.status(), 200);

    // The verdict can only change if the failed dispatch discarded the cached
    // one: the TTL has not come close to elapsing.
    assert_eq!(
        service.readiness().await.providers().get("codex"),
        Some(&ProviderAdmissionState::Unauthenticated),
        "a failed dispatch must retire the cached verdict rather than let it stand for the TTL"
    );
}
