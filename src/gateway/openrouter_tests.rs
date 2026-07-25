use super::{
    openrouter::{OpenRouterRequest, OpenRouterRequestError},
    routing::ModelIntent,
    test_support::{gateway_config, gateway_request},
};
use serde_json::json;

#[test]
fn auto_request_preserves_fields_and_appends_authoritative_router_plugin() {
    let config = gateway_config(
        "http://127.0.0.1:8080",
        "",
        r#"
auto_model = "openrouter/auto-beta"
cost_quality_tradeoff = 3
allowed_models = ["deepseek/*", "google/*"]
"#,
        "",
    );
    let original = json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "plugins": [{"id": "response-healing", "future_field": true}],
        "future_openrouter_field": {"preserve": [1, 2, 3]}
    });

    let body = OpenRouterRequest::build(
        gateway_request(original),
        &ModelIntent::Auto,
        config.openrouter(),
    )
    .expect("OpenRouter body")
    .into_body();

    assert_eq!(body["model"], json!("openrouter/auto-beta"));
    assert_eq!(
        body["future_openrouter_field"],
        json!({"preserve": [1, 2, 3]})
    );
    assert_eq!(
        body["plugins"],
        json!([
            {"id": "response-healing", "future_field": true},
            {
                "id": "auto-router",
                "cost_quality_tradeoff": 3,
                "allowed_models": ["deepseek/*", "google/*"]
            }
        ])
    );
}

#[test]
fn existing_auto_router_plugin_is_merged_without_duplication() {
    let config = gateway_config(
        "http://127.0.0.1:8080",
        "",
        r#"
cost_quality_tradeoff = 9
allowed_models = ["anthropic/*"]
"#,
        "",
    );
    let original = json!({
        "model": "cloud",
        "messages": [{"role": "user", "content": "hello"}],
        "plugins": [
            {
                "id": "auto-router",
                "cost_quality_tradeoff": 1,
                "allowed_models": ["caller/*"],
                "future_option": "preserved"
            },
            {"id": "response-healing"}
        ]
    });

    let body = OpenRouterRequest::build(
        gateway_request(original),
        &ModelIntent::CloudAuto,
        config.openrouter(),
    )
    .expect("OpenRouter body")
    .into_body();

    assert_eq!(
        body["plugins"],
        json!([
            {
                "id": "auto-router",
                "cost_quality_tradeoff": 9,
                "allowed_models": ["anthropic/*"],
                "future_option": "preserved"
            },
            {"id": "response-healing"}
        ])
    );
}

#[test]
fn empty_config_allowlist_removes_caller_override_for_auto_routing() {
    let config = gateway_config("http://127.0.0.1:8080", "", "", "");
    let original = json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "plugins": [{
            "id": "auto-router",
            "allowed_models": ["caller/*"],
            "future_option": true
        }]
    });

    let body = OpenRouterRequest::build(
        gateway_request(original),
        &ModelIntent::Auto,
        config.openrouter(),
    )
    .expect("OpenRouter body")
    .into_body();
    let plugin = &body["plugins"][0];

    assert!(plugin.get("allowed_models").is_none());
    assert_eq!(plugin["future_option"], json!(true));
    assert_eq!(plugin["cost_quality_tradeoff"], json!(9));
}

#[test]
fn explicit_cloud_model_only_patches_model_and_preserves_plugins() {
    let config = gateway_config("http://127.0.0.1:8080", "", "", "");
    let original = json!({
        "model": "deepseek/deepseek-v4-flash",
        "messages": [{"role": "user", "content": "hello"}],
        "plugins": [{"id": "auto-router", "cost_quality_tradeoff": 2}]
    });

    let body = OpenRouterRequest::build(
        gateway_request(original.clone()),
        &ModelIntent::CloudModel("deepseek/deepseek-v4-flash".to_string()),
        config.openrouter(),
    )
    .expect("OpenRouter body")
    .into_body();

    assert_eq!(body, original);
}

#[test]
fn malformed_plugins_are_rejected_safely_when_auto_policy_must_be_applied() {
    let config = gateway_config("http://127.0.0.1:8080", "", "", "");
    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "private prompt"}],
        "plugins": {"id": "auto-router"}
    }));

    let error = OpenRouterRequest::build(request, &ModelIntent::Auto, config.openrouter())
        .expect_err("plugins must be an array");

    assert_eq!(error, OpenRouterRequestError::InvalidPlugins);
    assert!(!error.to_string().contains("private prompt"));
}
