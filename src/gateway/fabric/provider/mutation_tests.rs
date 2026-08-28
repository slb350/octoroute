//! Mutation discriminators for provider request preparation and lease identity.

use super::*;
use crate::gateway::fabric::FabricConfig;
use secrecy::SecretString;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy)]
struct ProviderEnvironment;

impl Environment for ProviderEnvironment {
    fn get(&self, name: &str) -> Option<SecretString> {
        match name {
            "ZAI_API_KEY" => Some(SecretString::from("zai-test-key".to_string())),
            "KIMI_API_KEY" => Some(SecretString::from("kimi-test-key".to_string())),
            _ => None,
        }
    }
}

fn config() -> FabricConfig {
    FabricConfig::from_toml(include_str!("../../../../config.toml")).expect("repository config")
}

fn request(body: Value) -> GatewayRequest {
    GatewayRequest::parse(&serde_json::to_vec(&body).expect("request JSON"))
        .expect("gateway request")
}

fn built_body(config: &ProviderConfig, body: Value) -> Value {
    serde_json::from_slice(
        &build_open_ai_body(config, &request(body)).expect("OpenAI provider body"),
    )
    .expect("provider JSON")
}

#[test]
fn provider_defaults_never_overwrite_caller_controls() {
    let mut provider = config().providers["zai"].clone();
    provider.reasoning_effort = Some(ReasoningEffort::High);
    provider.temperature = Some(0.7);
    provider.max_tokens = Some(8_192);

    let caller_values = built_body(
        &provider,
        json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "keep my controls"}],
            "reasoning_effort": "low",
            "temperature": 0.25,
            "max_tokens": 321
        }),
    );
    assert_eq!(caller_values["reasoning_effort"], "low");
    assert_eq!(caller_values["temperature"], 0.25);
    assert_eq!(caller_values["max_tokens"], 321);

    let completion_alias = built_body(
        &provider,
        json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "keep the alias"}],
            "max_completion_tokens": 654
        }),
    );
    assert_eq!(completion_alias["max_completion_tokens"], 654);
    assert!(completion_alias.get("max_tokens").is_none());
}

#[test]
fn provider_reasoning_default_applies_only_without_a_caller_control() {
    let mut provider = config().providers["zai"].clone();
    provider.reasoning_effort = Some(ReasoningEffort::High);

    let without_control = built_body(
        &provider,
        json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "use the provider default"}]
        }),
    );
    assert_eq!(without_control["reasoning_effort"], "high");

    let with_nested_control = built_body(
        &provider,
        json!({
            "model": "cloud-sota",
            "messages": [{"role": "user", "content": "use my reasoning control"}],
            "reasoning": {"enabled": true}
        }),
    );
    assert_eq!(with_nested_control["reasoning"], json!({"enabled": true}));
    assert!(with_nested_control.get("reasoning_effort").is_none());
}

fn registry(config: &FabricConfig) -> ProviderRegistry {
    let metrics = Arc::new(FabricMetrics::new(config));
    ProviderRegistry::new(
        &config.providers,
        Arc::new(ProviderEnvironment),
        metrics,
        crate::gateway::http_client::build().expect("HTTP client"),
    )
    .expect("provider registry")
}

#[tokio::test]
async fn admitted_lease_reports_the_configured_destination_model() {
    let config = config();
    let registry = registry(&config);
    let request = request(json!({
        "model": "cloud-sota",
        "messages": [{"role": "user", "content": "identify the destination"}]
    }));

    let outcome = registry
        .try_admit("zai", &request, ReasoningEffort::Medium)
        .await
        .expect("provider admission");
    let ProviderAdmissionOutcome::Admitted(lease) = outcome else {
        panic!("provider must admit")
    };

    assert_eq!(lease.model(), config.providers["zai"].model);
}

#[tokio::test]
async fn adapter_incompatibility_is_an_admission_state_for_each_protocol() {
    let config = config();
    let registry = registry(&config);
    let anthropic_request = request(json!({
        "model": "cloud-sota",
        "messages": [{"role": "user", "content": "unsupported field"}],
        "logprobs": true
    }));
    let codex_request = request(json!({
        "model": "cloud-sota",
        "messages": [{"role": "user", "content": "two answers"}],
        "n": 2
    }));

    for (provider, request) in [("kimi", &anthropic_request), ("codex", &codex_request)] {
        let outcome = registry
            .try_admit(provider, request, ReasoningEffort::Medium)
            .await
            .expect("provider admission");
        assert!(
            matches!(
                outcome,
                ProviderAdmissionOutcome::Rejected(ProviderAdmissionState::Incompatible)
            ),
            "{provider} must report adapter incompatibility as an admission state"
        );
    }
}

#[test]
fn adapter_build_errors_distinguish_caller_incompatibility_from_gateway_failure() {
    assert!(matches!(
        classify_anthropic_build_error(anthropic::AnthropicAdapterError::Incompatible("fixture")),
        Ok(ProviderAdmissionOutcome::Rejected(
            ProviderAdmissionState::Incompatible
        ))
    ));
    assert!(matches!(
        classify_anthropic_build_error(anthropic::AnthropicAdapterError::Serialization),
        Err(ProviderRequestError::Anthropic)
    ));
    assert!(matches!(
        classify_codex_build_error(codex::CodexAdapterError::Incompatible),
        Ok(ProviderAdmissionOutcome::Rejected(
            ProviderAdmissionState::Incompatible
        ))
    ));
    assert!(matches!(
        classify_codex_build_error(codex::CodexAdapterError::Missing),
        Err(ProviderRequestError::Codex)
    ));
}
