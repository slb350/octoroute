use super::{
    config::{GatewayConfig, LocalCapability},
    request::GatewayRequest,
    routing::{
        LocalAdmissionState, ModelIntent, PrivacyDirective, RouteDecision, RouteDestination,
        RoutePlan, RoutePolicy, RoutePolicyError, RouteReason,
    },
    test_support::TestEnvironment,
};
use proptest::prelude::*;

fn resolve_plan(
    policy: &RoutePolicy<'_>,
    request: &GatewayRequest,
    intent: &ModelIntent,
    privacy: PrivacyDirective,
    local_state: LocalAdmissionState,
) -> Result<RouteDecision, RoutePolicyError> {
    policy.plan(request, intent, privacy)?.resolve(local_state)
}

fn config(capabilities: &[&str], default: &str) -> GatewayConfig {
    let capabilities = capabilities
        .iter()
        .map(|value| format!("\"{value}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let input = format!(
        r#"
config_version = 2

[server]
host = "127.0.0.1"
port = 3000
api_key_env = "OCTOROUTE_API_KEY"

[upstreams.local]
kind = "llama_cpp"
name = "local"
base_url = "http://127.0.0.1:8080"
	model = "example-local-model"
	model_revision = "test-model-revision"
	context_window = 65536
context_safety_tokens = 1024
max_in_flight = 1
capabilities = [{capabilities}]
health_path = "/health"
slots_path = "/slots?fail_on_no_slot=1"
input_tokens_path = "/v1/chat/completions/input_tokens"

[upstreams.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key_env = "OPENROUTER_API_KEY"

[routing]
default = "{default}"
"#
    );
    let environment = TestEnvironment::gateway();
    GatewayConfig::from_toml(&input, &environment).expect("valid route test config")
}

fn request(extra: &str) -> GatewayRequest {
    let body = format!(
        r#"{{
            "model": "auto",
            "messages": [{{"role": "user", "content": "hello"}}]
            {extra}
        }}"#
    );
    GatewayRequest::parse(body.as_bytes()).expect("valid request fixture")
}

#[test]
fn auto_uses_idle_compatible_local_model() {
    let config = config(&["chat", "stream"], "prefer_local");
    let request = request("");

    let decision = resolve_plan(
        &RoutePolicy::new(&config),
        &request,
        &ModelIntent::Auto,
        PrivacyDirective::None,
        LocalAdmissionState::Ready,
    )
    .expect("route decision");

    assert_eq!(decision.destination(), RouteDestination::Local);
    assert_eq!(decision.reason(), RouteReason::LocalCapable);
    assert!(decision.fallback_before_commit());
}

#[test]
fn auto_spills_busy_unhealthy_and_oversized_local_requests_to_cloud() {
    let config = config(&["chat"], "prefer_local");
    let policy = RoutePolicy::new(&config);
    let request = request("");
    let cases = [
        (LocalAdmissionState::Busy, RouteReason::LocalBusy),
        (LocalAdmissionState::Unhealthy, RouteReason::LocalUnhealthy),
        (
            LocalAdmissionState::ContextOverflow,
            RouteReason::LocalContextLimit,
        ),
    ];

    for (state, reason) in cases {
        let decision = resolve_plan(
            &policy,
            &request,
            &ModelIntent::Auto,
            PrivacyDirective::None,
            state,
        )
        .expect("cloud spill decision");
        assert_eq!(decision.destination(), RouteDestination::Cloud);
        assert_eq!(decision.reason(), reason);
        assert!(!decision.fallback_before_commit());
    }
}

#[test]
fn unsupported_features_route_auto_to_cloud() {
    let config = config(&["chat"], "prefer_local");
    let request = request(
        r#",
        "tools": [{"type": "function", "function": {"name": "lookup"}}]"#,
    );

    let decision = resolve_plan(
        &RoutePolicy::new(&config),
        &request,
        &ModelIntent::Auto,
        PrivacyDirective::None,
        LocalAdmissionState::Ready,
    )
    .expect("cloud route");

    assert_eq!(decision.destination(), RouteDestination::Cloud);
    assert_eq!(decision.reason(), RouteReason::LocalIncompatible);
}

#[test]
fn unknown_content_blocks_fail_closed_to_cloud() {
    let config = config(&["chat"], "prefer_local");
    let request = GatewayRequest::parse(
        br#"{
            "model": "auto",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "file",
                    "file": {"filename": "context.pdf", "file_data": "data:application/pdf;base64,AA=="}
                }]
            }]
        }"#,
    )
    .expect("valid file-content request");
    let policy = RoutePolicy::new(&config);

    assert!(matches!(
        policy
            .plan(&request, &ModelIntent::Auto, PrivacyDirective::None)
            .expect("automatic route"),
        RoutePlan::Cloud(_)
    ));
    let decision = resolve_plan(
        &policy,
        &request,
        &ModelIntent::Auto,
        PrivacyDirective::None,
        LocalAdmissionState::Ready,
    )
    .expect("cloud route");

    assert_eq!(decision.destination(), RouteDestination::Cloud);
    assert_eq!(decision.reason(), RouteReason::LocalIncompatible);
}

#[test]
fn malformed_message_shapes_fail_closed_for_automatic_and_forced_local_routes() {
    let config = config(
        &["chat", "tools", "image_input", "audio_input", "video_input"],
        "prefer_local",
    );
    let requests = [
        br#"{
            "model": "auto",
            "messages": ["not-an-object"]
        }"#
        .as_slice(),
        br#"{
            "model": "auto",
            "messages": [{"role": "user", "content": {"type": "text", "text": "hello"}}]
        }"#
        .as_slice(),
        br#"{
            "model": "auto",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "input_image",
                    "input_image": {"url": "https://example.com/a.png"}
                }]
            }]
        }"#
        .as_slice(),
    ];

    for body in requests {
        let request = GatewayRequest::parse(body).expect("minimally valid gateway request");
        let policy = RoutePolicy::new(&config);
        assert!(matches!(
            policy
                .plan(&request, &ModelIntent::Auto, PrivacyDirective::None)
                .expect("automatic malformed request routes safely"),
            RoutePlan::Cloud(RouteReason::LocalIncompatible)
        ));
        assert_eq!(
            policy
                .plan(&request, &ModelIntent::Local, PrivacyDirective::None)
                .expect_err("forced local rejects malformed message content"),
            RoutePolicyError::LocalIncompatible
        );
        assert_eq!(
            policy
                .plan(&request, &ModelIntent::Auto, PrivacyDirective::LocalOnly)
                .expect_err("local-only rejects malformed message content"),
            RoutePolicyError::LocalIncompatible
        );
    }
}

#[test]
fn tool_history_is_not_plain_local_chat() {
    let config = config(&["chat"], "prefer_local");
    let request = GatewayRequest::parse(
        br#"{
            "model": "auto",
            "messages": [{
                "role": "tool",
                "tool_call_id": "call_1",
                "content": "result"
            }]
        }"#,
    )
    .expect("valid tool-history request");

    assert!(matches!(
        RoutePolicy::new(&config)
            .plan(&request, &ModelIntent::Auto, PrivacyDirective::None)
            .expect("automatic tool history routes safely"),
        RoutePlan::Cloud(RouteReason::LocalIncompatible)
    ));
}

#[test]
fn configured_cloud_default_does_not_use_idle_local_model() {
    let config = config(&["chat"], "cloud");
    let request = request("");

    let decision = resolve_plan(
        &RoutePolicy::new(&config),
        &request,
        &ModelIntent::Auto,
        PrivacyDirective::None,
        LocalAdmissionState::Ready,
    )
    .expect("cloud default");

    assert_eq!(decision.destination(), RouteDestination::Cloud);
    assert_eq!(decision.reason(), RouteReason::CloudDefault);
}

#[test]
fn explicit_local_and_local_only_never_return_cloud() {
    let config = config(&["chat"], "prefer_local");
    let policy = RoutePolicy::new(&config);
    let request = request("");

    for intent in [ModelIntent::Local, ModelIntent::Auto] {
        for privacy in [PrivacyDirective::None, PrivacyDirective::LocalOnly] {
            for state in [
                LocalAdmissionState::Ready,
                LocalAdmissionState::Busy,
                LocalAdmissionState::Unhealthy,
                LocalAdmissionState::ContextOverflow,
            ] {
                if intent == ModelIntent::Auto && privacy == PrivacyDirective::None {
                    continue;
                }
                let result = resolve_plan(&policy, &request, &intent, privacy, state);
                match result {
                    Ok(decision) => {
                        assert_eq!(decision.destination(), RouteDestination::Local);
                        assert!(!decision.fallback_before_commit());
                    }
                    Err(error) => assert!(matches!(
                        error,
                        RoutePolicyError::LocalUnavailable { .. }
                            | RoutePolicyError::LocalIncompatible
                    )),
                }
            }
        }
    }
}

#[test]
fn contradictory_cloud_and_local_only_intent_is_rejected() {
    let config = config(&["chat"], "prefer_local");
    let error = resolve_plan(
        &RoutePolicy::new(&config),
        &request(""),
        &ModelIntent::CloudAuto,
        PrivacyDirective::LocalOnly,
        LocalAdmissionState::Ready,
    )
    .expect_err("contradictory route intent");

    assert_eq!(error, RoutePolicyError::ContradictoryIntent);
}

#[test]
fn explicit_local_rejects_unsupported_capabilities_instead_of_spilling() {
    let config = config(&["chat"], "prefer_local");
    let request = request(r#", "stream": true"#);
    assert!(!config.local().supports(LocalCapability::Stream));

    let error = resolve_plan(
        &RoutePolicy::new(&config),
        &request,
        &ModelIntent::Local,
        PrivacyDirective::None,
        LocalAdmissionState::Ready,
    )
    .expect_err("explicit local cannot accept unsupported feature");

    assert_eq!(error, RoutePolicyError::LocalIncompatible);
}

#[test]
fn only_routes_that_can_choose_local_require_admission_probes() {
    let prefer_local = config(&["chat"], "prefer_local");
    let policy = RoutePolicy::new(&prefer_local);
    let compatible = request("");
    let incompatible = request(r#", "stream": true"#);

    assert!(matches!(
        policy
            .plan(&compatible, &ModelIntent::Auto, PrivacyDirective::None)
            .expect("auto route"),
        RoutePlan::Local(_)
    ));
    assert!(matches!(
        policy
            .plan(&compatible, &ModelIntent::Local, PrivacyDirective::None)
            .expect("local route"),
        RoutePlan::Local(_)
    ));
    assert!(matches!(
        policy
            .plan(&compatible, &ModelIntent::CloudAuto, PrivacyDirective::None)
            .expect("cloud route"),
        RoutePlan::Cloud(_)
    ));
    assert!(matches!(
        policy
            .plan(&incompatible, &ModelIntent::Auto, PrivacyDirective::None)
            .expect("incompatible auto route"),
        RoutePlan::Cloud(_)
    ));

    let cloud_default = config(&["chat"], "cloud");
    assert!(matches!(
        RoutePolicy::new(&cloud_default)
            .plan(&compatible, &ModelIntent::Auto, PrivacyDirective::None)
            .expect("cloud default"),
        RoutePlan::Cloud(_)
    ));
}

proptest! {
    #[test]
    fn every_automatic_request_produces_one_bounded_typed_decision(
        state in prop::sample::select(vec![
            LocalAdmissionState::Ready,
            LocalAdmissionState::Busy,
            LocalAdmissionState::Unhealthy,
            LocalAdmissionState::ContextOverflow,
        ]),
        request_streams in any::<bool>(),
        local_streams in any::<bool>(),
        cloud_is_default in any::<bool>(),
    ) {
        let capabilities = if local_streams {
            vec!["chat", "stream"]
        } else {
            vec!["chat"]
        };
        let route_default = if cloud_is_default { "cloud" } else { "prefer_local" };
        let config = config(&capabilities, route_default);
        let request = if request_streams {
            request(r#", "stream": true"#)
        } else {
            request("")
        };

        let decision = resolve_plan(
            &RoutePolicy::new(&config),
            &request,
            &ModelIntent::Auto,
            PrivacyDirective::None,
            state,
        )
        .expect("automatic requests always have one destination");

        prop_assert!(matches!(
            decision.destination(),
            RouteDestination::Local | RouteDestination::Cloud
        ));
        prop_assert!([
            "local_capable",
            "local_incompatible",
            "local_context_limit",
            "local_busy",
            "local_unhealthy",
            "cloud_default",
        ].contains(&decision.reason().as_str()));
    }

    #[test]
    fn local_only_property_never_produces_cloud(
        state in prop::sample::select(vec![
            LocalAdmissionState::Ready,
            LocalAdmissionState::Busy,
            LocalAdmissionState::Unhealthy,
            LocalAdmissionState::ContextOverflow,
        ]),
    ) {
        let config = config(&["chat"], "prefer_local");
        let request = request("");
        let result = resolve_plan(
            &RoutePolicy::new(&config),
            &request,
            &ModelIntent::Auto,
            PrivacyDirective::LocalOnly,
            state,
        );

        let stayed_local = match result {
            Ok(decision) => decision.destination() == RouteDestination::Local,
            Err(_) => true,
        };
        prop_assert!(stayed_local);
    }
}
