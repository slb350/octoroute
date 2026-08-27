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
            .body_bytes_for_model("example-local-model")
            .expect("model mutation"),
    )
    .expect("serialized local body");

    let mut expected = original;
    expected["model"] = json!("example-local-model");
    assert_eq!(local, expected);
}

#[test]
fn session_policy_uses_only_bounded_non_control_identifiers() {
    let valid = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "session_id": "session-123"
    }));
    assert_eq!(valid.session_id(), Some("session-123"));

    for session_id in [
        json!(""),
        json!("bad\nsession"),
        json!("x".repeat(129)),
        json!(42),
    ] {
        let request = gateway_request(json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hello"}],
            "session_id": session_id
        }));
        assert_eq!(request.session_id(), None);
    }
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
fn malformed_message_and_content_shapes_fail_closed() {
    let malformed_messages = [
        json!(["not-an-object"]),
        json!([{"content": "missing role"}]),
        json!([{"role": "future-role", "content": "unknown role"}]),
        json!([{"role": "user"}]),
        json!([{"role": "assistant", "content": null}]),
        json!([{"role": "user", "content": {"type": "text", "text": "hello"}}]),
        json!([{"role": "user", "content": [{"type": "text", "text": 42}]}]),
        json!([{"role": "user", "content": []}]),
        json!([{"role": "assistant", "content": null, "tool_calls": []}]),
        json!([{"role": "tool", "content": "missing tool-call id"}]),
        json!([{
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": 42,
                "type": "function",
                "function": {"name": "lookup", "arguments": "{}"}
            }]
        }]),
    ];

    for messages in malformed_messages {
        let request = gateway_request(json!({
            "model": "auto",
            "messages": messages
        }));

        assert!(
            request
                .features()
                .contains(&RequestFeature::UnsupportedContent),
            "malformed messages were treated as local-compatible: {messages}"
        );
    }
}

#[test]
fn tool_history_requires_local_tool_capability() {
    let requests = [
        json!({
            "model": "auto",
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{}"}
                }]
            }]
        }),
        json!({
            "model": "auto",
            "messages": [{
                "role": "tool",
                "tool_call_id": "call_1",
                "content": "result"
            }]
        }),
    ];

    for body in requests {
        let request = gateway_request(body);
        assert!(
            request
                .features()
                .contains(&RequestFeature::Capability(LocalCapability::Tools))
        );
        assert!(
            !request
                .features()
                .contains(&RequestFeature::UnsupportedContent)
        );
    }
}

#[test]
fn only_verified_llama_cpp_content_block_names_are_local_capabilities() {
    let unsupported_blocks = [
        json!({"type": "input_image", "input_image": {"url": "https://example.com/a.png"}}),
        json!({"type": "audio", "audio": {"data": "AA=="}}),
        json!({"type": "video", "video": {"data": "AA=="}}),
    ];

    for block in unsupported_blocks {
        let request = gateway_request(json!({
            "model": "auto",
            "messages": [{
                "role": "user",
                "content": [block]
            }]
        }));

        assert!(
            request
                .features()
                .contains(&RequestFeature::UnsupportedContent),
            "unverified content block alias was accepted"
        );
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
