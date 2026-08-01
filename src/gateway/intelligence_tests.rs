use super::{
    service_tests::{
        FakeResult, FakeTransport, authorized_headers, body_with_prompt,
        mount_auto_local_admission, mount_input_tokens, mount_intelligent_response,
        mount_intelligent_route, mount_local_admission, service,
    },
    test_support::gateway_config,
};
use axum::http::StatusCode;
use serde_json::json;
use wiremock::MockServer;

#[tokio::test]
async fn auto_routes_hard_work_to_openrouter_auto_even_when_local_is_idle() {
    let local = MockServer::start().await;
    mount_intelligent_route(&local, "cloud", 1).await;
    let config = gateway_config(&local.uri(), "", "", "semantic_mode = \"enforced\"");
    let transport = FakeTransport::default()
        .with_local(FakeResult::Response(
            StatusCode::OK,
            r#"{"model":"puzzle-75b","choices":[]}"#,
        ))
        .with_cloud(FakeResult::Response(
            StatusCode::OK,
            r#"{"model":"anthropic/claude-opus-4.8","choices":[]}"#,
        ));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(
            &authorized_headers(),
            body_with_prompt(
                "auto",
                "Design and prove a lock-free concurrent B-tree with linearizable snapshots.",
            ),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "cloud");
    assert_eq!(response.headers()["x-octoroute-reason"], "cloud_quality");
    assert_eq!(transport.local_calls(), 0);
    assert_eq!(transport.cloud_calls(), 1);
    assert_eq!(transport.cloud_models(), ["openrouter/auto"]);

    let requests = local.received_requests().await.expect("received requests");
    let classifier: serde_json::Value = requests
        .iter()
        .find(|request| {
            request.method.as_str() == "POST" && request.url.path() == "/v1/chat/completions"
        })
        .map(|request| serde_json::from_slice(&request.body).expect("classifier JSON"))
        .expect("classifier request");
    assert_eq!(classifier["model"], "puzzle-75b");
    assert_eq!(classifier["stream"], false);
    assert_eq!(classifier["chat_template_kwargs"]["enable_thinking"], false);
    assert_eq!(
        classifier["response_format"]["json_schema"]["schema"]["properties"]["destination"]["enum"],
        json!(["local", "cloud"])
    );
}

#[tokio::test]
async fn auto_keeps_routine_work_local_after_intelligent_classification() {
    let local = MockServer::start().await;
    mount_auto_local_admission(&local).await;
    let config = gateway_config(&local.uri(), "", "", "semantic_mode = \"enforced\"");
    let transport = FakeTransport::default().with_local(FakeResult::Response(
        StatusCode::OK,
        r#"{"model":"puzzle-75b","choices":[]}"#,
    ));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(
            &authorized_headers(),
            body_with_prompt(
                "auto",
                "Rewrite this sentence to sound friendlier: Send the report today.",
            ),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "local");
    assert_eq!(response.headers()["x-octoroute-reason"], "local_capable");
    assert_eq!(transport.local_calls(), 1);
    assert_eq!(transport.cloud_calls(), 0);
}

#[tokio::test]
async fn invalid_semantic_decision_fails_safely_to_openrouter_auto() {
    let local = MockServer::start().await;
    mount_intelligent_response(&local, "not-a-routing-decision", 1).await;
    let config = gateway_config(&local.uri(), "", "", "semantic_mode = \"enforced\"");
    let transport = FakeTransport::default().with_cloud(FakeResult::Response(
        StatusCode::OK,
        r#"{"model":"openai/gpt-5.2","choices":[]}"#,
    ));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(
            &authorized_headers(),
            body_with_prompt("auto", "An ambiguous request"),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "cloud");
    assert_eq!(response.headers()["x-octoroute-reason"], "router_failure");
    assert_eq!(transport.local_calls(), 0);
    assert_eq!(transport.cloud_calls(), 1);
    assert_eq!(transport.cloud_models(), ["openrouter/auto"]);
}

#[tokio::test]
async fn shadow_mode_observes_cloud_but_keeps_compatible_work_local() {
    let local = MockServer::start().await;
    mount_intelligent_route(&local, "cloud", 1).await;
    mount_input_tokens(&local, 10).await;
    let config = gateway_config(&local.uri(), "", "", "semantic_mode = \"shadow\"");
    let transport = FakeTransport::default().with_local(FakeResult::Response(
        StatusCode::OK,
        r#"{"model":"puzzle-75b","choices":[]}"#,
    ));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(
            &authorized_headers(),
            body_with_prompt("auto", "A task the classifier escalates"),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "local");
    assert_eq!(response.headers()["x-octoroute-reason"], "local_capable");
    assert_eq!(transport.local_calls(), 1);
    assert_eq!(transport.cloud_calls(), 0);
    assert!(
        gateway
            .metrics_text()
            .expect("metrics")
            .contains("octoroute_semantic_decisions_total{mode=\"shadow\",outcome=\"cloud\"} 1")
    );
}

#[tokio::test]
async fn shadow_mode_router_failure_does_not_override_local_admission() {
    let local = MockServer::start().await;
    mount_intelligent_response(&local, "invalid", 1).await;
    mount_input_tokens(&local, 10).await;
    let config = gateway_config(&local.uri(), "", "", "semantic_mode = \"shadow\"");
    let transport = FakeTransport::default().with_local(FakeResult::Response(
        StatusCode::OK,
        r#"{"model":"puzzle-75b","choices":[]}"#,
    ));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(
            &authorized_headers(),
            body_with_prompt("auto", "A task with an invalid shadow decision"),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "local");
    assert_eq!(transport.local_calls(), 1);
    assert_eq!(transport.cloud_calls(), 0);
    assert!(
        gateway
            .metrics_text()
            .expect("metrics")
            .contains("octoroute_semantic_decisions_total{mode=\"shadow\",outcome=\"failure\"} 1")
    );
}

#[tokio::test]
async fn disabled_mode_skips_semantic_routing() {
    let local = MockServer::start().await;
    mount_local_admission(&local).await;
    let config = gateway_config(&local.uri(), "", "", "semantic_mode = \"disabled\"");
    let transport = FakeTransport::default().with_local(FakeResult::Response(
        StatusCode::OK,
        r#"{"model":"puzzle-75b","choices":[]}"#,
    ));
    let gateway = service(config, transport.clone());

    let response = gateway
        .handle_chat(
            &authorized_headers(),
            body_with_prompt("auto", "A task without semantic routing"),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-octoroute-destination"], "local");
    assert_eq!(transport.local_calls(), 1);
    assert_eq!(transport.cloud_calls(), 0);
    assert!(
        local
            .received_requests()
            .await
            .expect("received requests")
            .iter()
            .all(|request| request.url.path() != "/v1/chat/completions")
    );
    assert!(
        !gateway
            .metrics_text()
            .expect("metrics")
            .contains("octoroute_semantic_decisions_total{mode=\"disabled\"")
    );
}
