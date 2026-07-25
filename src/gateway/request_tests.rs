use super::{
    config::LocalCapability,
    request::{GatewayRequest, GatewayRequestError, RequestFeature},
    test_support::gateway_request,
};
use serde_json::json;

#[test]
fn preserves_unknown_fields_when_patching_only_the_model() {
    let original = json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "stream": false,
        "future_openai_field": {
            "nested": [1, 2, {"kept": true}]
        },
        "top_k": 41
    });
    let request = gateway_request(original.clone());
    let local = serde_json::from_slice::<serde_json::Value>(
        &request
            .body_bytes_for_model("puzzle-75b")
            .expect("model mutation"),
    )
    .expect("serialized local body");

    let mut expected = original;
    expected["model"] = json!("puzzle-75b");
    assert_eq!(local, expected);
}

#[test]
fn detects_all_features_without_flattening_messages() {
    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "describe this"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA=="}},
                {"type": "input_audio", "input_audio": {"data": "AA=="}},
                {"type": "input_video", "input_video": {"data": "AA=="}}
            ]
        }],
        "stream": true,
        "tools": [{"type": "function", "function": {"name": "lookup"}}],
        "response_format": {"type": "json_schema", "json_schema": {"name": "result"}},
        "reasoning": {"effort": "medium"},
        "plugins": [{"id": "web"}]
    }));

    for feature in [
        RequestFeature::Capability(LocalCapability::Chat),
        RequestFeature::Capability(LocalCapability::Stream),
        RequestFeature::Capability(LocalCapability::Tools),
        RequestFeature::Capability(LocalCapability::StructuredOutput),
        RequestFeature::Capability(LocalCapability::ImageInput),
        RequestFeature::Capability(LocalCapability::AudioInput),
        RequestFeature::Capability(LocalCapability::VideoInput),
        RequestFeature::Capability(LocalCapability::Reasoning),
        RequestFeature::OpenRouterPlugins,
    ] {
        assert!(request.features().contains(&feature), "missing {feature:?}");
    }
}

#[test]
fn absent_stream_does_not_request_local_streaming_capability() {
    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}]
    }));

    assert!(
        !request
            .features()
            .contains(&RequestFeature::Capability(LocalCapability::Stream))
    );
}

#[test]
fn rejects_invalid_minimum_envelope_without_echoing_body() {
    let cases = [
        (
            json!({"messages": [{"role": "user", "content": "hello"}]}),
            "model",
        ),
        (json!({"model": "auto"}), "messages"),
        (json!({"model": "auto", "messages": []}), "messages"),
        (
            json!({
                "model": "auto",
                "messages": [{"role": "user", "content": "private-value"}],
                "stream": "yes"
            }),
            "stream",
        ),
    ];

    for (value, expected_field) in cases {
        let bytes = serde_json::to_vec(&value).expect("serialize invalid fixture");
        let error = GatewayRequest::parse(&bytes).expect_err("fixture must fail");

        assert!(matches!(
            error,
            GatewayRequestError::Invalid { ref field, .. } if field == expected_field
        ));
        assert!(!error.to_string().contains("private-value"));
    }
}

#[test]
fn rejects_non_json_and_non_object_bodies_safely() {
    assert!(matches!(
        GatewayRequest::parse(b"not-json"),
        Err(GatewayRequestError::Json { .. })
    ));
    assert!(matches!(
        GatewayRequest::parse(br#"["not", "an", "object"]"#),
        Err(GatewayRequestError::Invalid { ref field, .. }) if field == "body"
    ));
}

#[test]
fn output_budget_prefers_max_completion_tokens_and_defaults_when_absent() {
    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 2000,
        "max_completion_tokens": 1000
    }));

    assert_eq!(
        request.output_token_budget(4096).expect("valid budget"),
        1000
    );

    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}]
    }));
    assert_eq!(
        request.output_token_budget(4096).expect("default budget"),
        4096
    );
}

#[test]
fn output_budget_treats_null_optional_limits_as_absent() {
    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 2000,
        "max_completion_tokens": null
    }));
    assert_eq!(
        request
            .output_token_budget(4096)
            .expect("legacy limit after null preferred limit"),
        2000
    );

    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": null,
        "max_completion_tokens": null
    }));
    assert_eq!(
        request
            .output_token_budget(4096)
            .expect("default after null limits"),
        4096
    );
}

#[test]
fn output_budget_rejects_zero_or_non_integer_limits() {
    for (field, value) in [
        ("max_tokens", json!(0)),
        ("max_completion_tokens", json!(1.5)),
    ] {
        let request = gateway_request(json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hello"}],
            field: value
        }));
        let error = request
            .output_token_budget(4096)
            .expect_err("invalid output limit");
        assert!(matches!(
            error,
            GatewayRequestError::Invalid {
                field: ref actual,
                ..
            } if actual == field
        ));
    }
}
