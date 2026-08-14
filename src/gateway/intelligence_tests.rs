use super::{
    service_tests::{
        FakeResult, FakeTransport, authorized_headers, body_with_prompt,
        mount_auto_local_admission, mount_input_tokens, mount_input_tokens_count,
        mount_intelligent_forecast, mount_intelligent_forecast_count, mount_intelligent_response,
        mount_intelligent_route, mount_local_admission, service,
    },
    test_support::{
        gateway_config, gateway_config_with_local_capabilities, trajectory_tool_call,
        trajectory_tool_result,
    },
};
use axum::http::{HeaderValue, StatusCode};
use bytes::Bytes;
use serde_json::json;
use wiremock::MockServer;

fn body_with_trajectory(model: &str) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "model": model,
            "messages": [
                trajectory_tool_call("call-1"),
                trajectory_tool_result("call-1", json!({
            "outcome": "failure",
            "error_severity": "hard",
            "environment": "production",
            "test_status": "failed",
            "context_compacted": true
                })),
                {"role": "user", "content": "Recover and continue."}
            ]
        }))
        .expect("serialize trajectory request"),
    )
}

fn body_with_session(model: &str, session_id: &str) -> Bytes {
    Bytes::from(
        serde_json::to_vec(&json!({
            "model": model,
            "session_id": session_id,
            "messages": [{"role": "user", "content": "Continue this session."}]
        }))
        .expect("serialize session request"),
    )
}

#[tokio::test]
async fn repeated_hard_forecasts_latch_only_automatic_session_traffic() {
    let local = MockServer::start().await;
    mount_intelligent_forecast_count(&local, 0.1, "unsupported", "known_local_limit", 2, 4).await;
    mount_input_tokens_count(&local, 10, 2).await;
    let config = gateway_config(
        &local.uri(),
        "",
        "",
        "semantic_mode = \"enforced\"\nsession_latch_enabled = true\nsession_latch_evidence_threshold = 2",
    );
    let transport = FakeTransport::default()
        .with_cloud(FakeResult::Response(StatusCode::OK, r#"{"choices":[]}"#))
        .with_cloud(FakeResult::Response(StatusCode::OK, r#"{"choices":[]}"#))
        .with_cloud(FakeResult::Response(StatusCode::OK, r#"{"choices":[]}"#))
        .with_local(FakeResult::Response(StatusCode::OK, r#"{"choices":[]}"#))
        .with_local(FakeResult::Response(StatusCode::OK, r#"{"choices":[]}"#));
    let gateway = service(config, transport.clone());

    for expected_reason in ["cloud_quality", "cloud_quality", "session_cloud_latch"] {
        let response = gateway
            .handle_chat(
                &authorized_headers(),
                body_with_session("auto", "private-session"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-octoroute-destination"], "cloud");
        assert_eq!(response.headers()["x-octoroute-reason"], expected_reason);
    }

    let explicit = gateway
        .handle_chat(
            &authorized_headers(),
            body_with_session("local", "private-session"),
        )
        .await;
    assert_eq!(explicit.status(), StatusCode::OK);
    assert_eq!(explicit.headers()["x-octoroute-destination"], "local");
    assert_eq!(explicit.headers()["x-octoroute-reason"], "explicit_local");

    let mut local_only_headers = authorized_headers();
    local_only_headers.insert(
        "x-octoroute-privacy",
        HeaderValue::from_static("local-only"),
    );
    let local_only = gateway
        .handle_chat(
            &local_only_headers,
            body_with_session("auto", "private-session"),
        )
        .await;
    assert_eq!(local_only.status(), StatusCode::OK);
    assert_eq!(local_only.headers()["x-octoroute-destination"], "local");
    assert_eq!(local_only.headers()["x-octoroute-reason"], "local_only");

    assert_eq!(transport.cloud_calls(), 3);
    assert_eq!(transport.local_calls(), 2);
}

async fn assert_trajectory_forecast_context(mode: &str, expects_trajectory: bool) {
    let local = MockServer::start().await;
    mount_intelligent_forecast(&local, 0.9, "supported", "bounded_verification", 1).await;
    mount_input_tokens(&local, 10).await;
    let config = gateway_config_with_local_capabilities(
        &local.uri(),
        r#"["chat", "stream", "tools"]"#,
        "",
        &format!("semantic_mode = \"{mode}\""),
    );
    let gateway = service(
        config,
        FakeTransport::default().with_local(FakeResult::Response(
            StatusCode::OK,
            r#"{"model":"puzzle-75b","choices":[]}"#,
        )),
    );

    let response = gateway
        .handle_chat(&authorized_headers(), body_with_trajectory("auto"))
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let requests = local.received_requests().await.expect("received requests");
    let classifier: serde_json::Value = requests
        .iter()
        .find(|request| {
            request.method.as_str() == "POST" && request.url.path() == "/v1/chat/completions"
        })
        .map(|request| serde_json::from_slice(&request.body).expect("classifier JSON"))
        .expect("classifier request");
    let prompt = classifier["messages"][1]["content"]
        .as_str()
        .expect("forecast prompt");
    let system_prompt = classifier["messages"][0]["content"]
        .as_str()
        .expect("forecast system prompt");
    assert_eq!(prompt.contains("<verified_trajectory>"), expects_trajectory);
    assert_eq!(
        system_prompt.contains("verified_trajectory block"),
        expects_trajectory
    );
    if expects_trajectory {
        assert!(prompt.contains(r#""error_severity":"hard""#));
        assert!(prompt.contains(r#""environment":"production""#));
        assert!(prompt.contains(r#""context_compacted":true"#));
    }
}

#[tokio::test]
async fn verified_trajectory_evidence_is_added_to_shadow_forecasts() {
    assert_trajectory_forecast_context("shadow", true).await;
}

#[tokio::test]
async fn enforced_forecasts_remain_free_of_trajectory_context() {
    assert_trajectory_forecast_context("enforced", false).await;
}

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
    let system_prompt = classifier["messages"][0]["content"]
        .as_str()
        .expect("semantic system prompt");
    assert!(system_prompt.contains("octoroute-strix-capability-card/v1"));
    assert!(system_prompt.contains(r#"upstream_name: "strix""#));
    assert!(system_prompt.contains(r#"model_alias: "puzzle-75b""#));
    assert!(system_prompt.contains(r#"enabled_capabilities: ["chat","stream"]"#));
    assert!(system_prompt.contains(r#"disabled_capabilities: ["tools","structured_output""#));
    assert!(system_prompt.contains("recursive SQL"));
    assert!(system_prompt.contains("Never infer difficulty from terse wording"));
    assert!(!system_prompt.contains(&local.uri()));
    let schema = &classifier["response_format"]["json_schema"]["schema"];
    assert_eq!(
        schema["required"],
        json!([
            "p_local_success",
            "capability_boundary",
            "primary_rule",
            "crux"
        ])
    );
    assert_eq!(schema["properties"]["p_local_success"]["minimum"], 0.0);
    assert_eq!(schema["properties"]["p_local_success"]["maximum"], 1.0);
    assert_eq!(
        schema["properties"]["capability_boundary"]["enum"],
        json!(["supported", "uncertain", "unsupported", "unmatched"])
    );
}

#[tokio::test]
async fn capability_boundary_adjusts_the_deterministic_local_threshold() {
    for (probability, boundary, rule, routing, expected_destination) in [
        (
            0.55,
            "supported",
            "bounded_verification",
            "semantic_mode = \"enforced\"",
            "local",
        ),
        (
            0.55,
            "uncertain",
            "ambiguous_requirements",
            "semantic_mode = \"enforced\"",
            "cloud",
        ),
        (
            0.60,
            "uncertain",
            "ambiguous_requirements",
            "semantic_mode = \"enforced\"",
            "local",
        ),
        (
            0.65,
            "supported",
            "bounded_verification",
            "semantic_mode = \"enforced\"\nlocal_success_threshold = 0.70",
            "cloud",
        ),
        (
            0.65,
            "unsupported",
            "known_local_limit",
            "semantic_mode = \"enforced\"",
            "cloud",
        ),
        (
            0.65,
            "unmatched",
            "no_matching_rule",
            "semantic_mode = \"enforced\"",
            "local",
        ),
    ] {
        let local = MockServer::start().await;
        mount_intelligent_forecast(&local, probability, boundary, rule, 1).await;
        if expected_destination == "local" {
            mount_input_tokens(&local, 10).await;
        }
        let config = gateway_config(&local.uri(), "", "", routing);
        let transport = FakeTransport::default()
            .with_local(FakeResult::Response(
                StatusCode::OK,
                r#"{"model":"puzzle-75b","choices":[]}"#,
            ))
            .with_cloud(FakeResult::Response(
                StatusCode::OK,
                r#"{"model":"openai/gpt-5.2","choices":[]}"#,
            ));
        let gateway = service(config, transport);

        let response = gateway
            .handle_chat(
                &authorized_headers(),
                body_with_prompt("auto", "Evaluate a boundary-sensitive task"),
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["x-octoroute-destination"],
            expected_destination
        );
    }
}

#[tokio::test]
async fn invalid_forecast_fails_safely_to_openrouter_auto() {
    for content in [
        r#"{"p_local_success":1.1,"capability_boundary":"supported","primary_rule":"bounded_verification","crux":"Impossible confidence."}"#,
        r#"{"p_local_success":0.8,"capability_boundary":"unsupported","primary_rule":"bounded_verification","crux":"Rule and boundary disagree."}"#,
        r#"{"p_local_success":0.8,"capability_boundary":"supported","primary_rule":"bounded_verification","crux":" "}"#,
        r#"{"p_local_success":0.8,"capability_boundary":"supported","primary_rule":"invented_rule","crux":"Unknown rule."}"#,
    ] {
        let local = MockServer::start().await;
        mount_intelligent_response(&local, content, 1).await;
        let config = gateway_config(&local.uri(), "", "", "semantic_mode = \"enforced\"");
        let transport = FakeTransport::default().with_cloud(FakeResult::Response(
            StatusCode::OK,
            r#"{"model":"openai/gpt-5.2","choices":[]}"#,
        ));
        let gateway = service(config, transport.clone());

        let response = gateway
            .handle_chat(
                &authorized_headers(),
                body_with_prompt("auto", "A task with an invalid forecast"),
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["x-octoroute-destination"], "cloud");
        assert_eq!(response.headers()["x-octoroute-reason"], "router_failure");
        assert_eq!(transport.cloud_calls(), 1);
    }
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
    let metrics = gateway.metrics_text().expect("metrics");
    assert!(
        metrics.contains("octoroute_semantic_decisions_total{mode=\"shadow\",outcome=\"cloud\"} 1")
    );
    assert!(metrics.contains(
        "octoroute_semantic_local_success_probability_count{boundary=\"supported\",mode=\"shadow\"} 1"
    ));
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
