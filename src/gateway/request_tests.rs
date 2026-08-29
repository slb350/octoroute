use super::{
    fabric::LocalCapability,
    request::{GatewayRequest, GatewayRequestError, RequestFeature},
};
use serde_json::json;

fn gateway_request(value: serde_json::Value) -> GatewayRequest {
    GatewayRequest::parse(&serde_json::to_vec(&value).expect("serialize request"))
        .expect("valid gateway request")
}

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
fn local_reasoning_default_applies_only_when_the_caller_omits_controls() {
    let omitted = gateway_request(json!({
        "model": "worker",
        "messages": [{"role": "user", "content": "hello"}]
    }));
    let body: serde_json::Value = serde_json::from_slice(
        &omitted
            .body_bytes_for_model_with_reasoning_default("local-model", "high")
            .expect("local body"),
    )
    .expect("body JSON");
    assert_eq!(body["reasoning_effort"], "high");

    let explicit = gateway_request(json!({
        "model": "worker",
        "messages": [{"role": "user", "content": "hello"}],
        "reasoning": {"effort": "low"}
    }));
    let body: serde_json::Value = serde_json::from_slice(
        &explicit
            .body_bytes_for_model_with_reasoning_default("local-model", "high")
            .expect("local body"),
    )
    .expect("body JSON");
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["reasoning"]["effort"], "low");
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
fn rejects_unbounded_or_unreachable_virtual_model_names() {
    for model in ["contains/slash".to_string(), "x".repeat(129)] {
        let bytes = serde_json::to_vec(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .expect("request JSON");
        let error = GatewayRequest::parse(&bytes).expect_err("model must be bounded");
        assert!(matches!(
            error,
            GatewayRequestError::Invalid { ref field, .. } if field == "model"
        ));
    }
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

/// llama.cpp's `oaicompat_chat_params_parse` overwrites the `max_tokens`-derived
/// `n_predict` with an explicit `n_predict`, on the very endpoint Octoroute
/// proxies. The budget local admission reserves has to be the number the member
/// will actually generate, not the one the OpenAI aliases suggest.
#[test]
fn output_budget_prefers_n_predict_over_the_openai_aliases() {
    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "n_predict": 500_000
    }));
    assert_eq!(
        request.output_token_budget(4096).expect("n_predict budget"),
        500_000
    );

    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 100,
        "max_completion_tokens": 200,
        "n_predict": 500_000
    }));
    assert_eq!(
        request
            .output_token_budget(4096)
            .expect("n_predict outranks both aliases"),
        500_000
    );

    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "max_completion_tokens": 1000,
        "n_predict": null
    }));
    assert_eq!(
        request
            .output_token_budget(4096)
            .expect("null n_predict falls through"),
        1000
    );
}

#[test]
fn output_budget_rejects_invalid_n_predict_like_the_other_limits() {
    for value in [json!(1.5), json!("512"), json!(u64::from(u32::MAX) + 1)] {
        let request = gateway_request(json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hello"}],
            "n_predict": value
        }));
        let error = request
            .output_token_budget(4096)
            .expect_err("invalid n_predict");
        assert!(
            matches!(
                error,
                GatewayRequestError::Invalid { field: ref actual, .. } if actual == "n_predict"
            ),
            "expected an invalid `n_predict`, got {error:?}"
        );
    }
}

/// Local admission must reserve the exact output ceiling forwarded to llama.cpp.
/// A negative `n_predict` is unlimited, so no finite context proof can admit it.
#[test]
fn a_negative_n_predict_is_rejected_as_unbounded() {
    for body in [
        json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hello"}],
            "n_predict": -1
        }),
        json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hello"}],
            "n_predict": -1,
            "max_tokens": 256
        }),
    ] {
        let request = gateway_request(body);
        let error = request
            .output_token_budget(4096)
            .expect_err("unbounded local generation must fail closed");
        assert!(matches!(
            error,
            GatewayRequestError::Invalid { field, .. } if field == "n_predict"
        ));
    }
}

/// llama.cpp documents `n_predict: 0` as "evaluate the prompt into the cache
/// without generating", which is a real reservation of zero output tokens
/// rather than a malformed limit.
#[test]
fn a_zero_n_predict_reserves_no_output_tokens() {
    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}],
        "n_predict": 0,
        "max_tokens": 4096
    }));
    assert_eq!(
        request
            .output_token_budget(4096)
            .expect("zero is a budget, not an error"),
        0
    );
}

/// `include_reasoning` gates the pool's `reasoning` capability, so a caller
/// using it has stated a reasoning intent. Adding the pool default alongside it
/// would attach an effort the caller never chose.
#[test]
fn include_reasoning_counts_as_a_caller_reasoning_control() {
    let request = gateway_request(json!({
        "model": "worker",
        "messages": [{"role": "user", "content": "hello"}],
        "include_reasoning": true
    }));
    let body: serde_json::Value = serde_json::from_slice(
        &request
            .body_bytes_for_model_with_reasoning_default("local-model", "high")
            .expect("local body"),
    )
    .expect("body JSON");

    assert_eq!(body["include_reasoning"], json!(true));
    assert!(
        body.get("reasoning_effort").is_none(),
        "the pool default must not be added beside a caller reasoning control: {body}"
    );
}

#[test]
fn request_debug_identifies_the_route_without_exposing_the_body() {
    let request = gateway_request(json!({
        "model": "private-route",
        "messages": [{"role": "user", "content": "hunter2"}]
    }));

    assert_eq!(
        format!("{request:?}"),
        "GatewayRequest { model: \"private-route\", body: \"[REDACTED]\" }"
    );
}

#[test]
fn only_non_text_modalities_require_non_text_output() {
    for (modalities, expected) in [(json!(["text"]), false), (json!(["text", "audio"]), true)] {
        let request = gateway_request(json!({
            "model": "auto",
            "messages": [{"role": "user", "content": "hello"}],
            "modalities": modalities
        }));

        assert_eq!(
            request.features().contains(&RequestFeature::NonTextOutput),
            expected,
            "unexpected feature inference for {modalities}"
        );
    }
}

#[test]
fn content_blocks_require_the_verified_shape() {
    let text = gateway_request(json!({
        "model": "auto",
        "messages": [{
            "role": "user",
            "content": [{"type": "text", "text": "hello"}]
        }]
    }));
    assert!(
        !text
            .features()
            .contains(&RequestFeature::UnsupportedContent)
    );

    for (block, capability) in [
        (
            json!({"type": "image_url", "image_url": {}}),
            LocalCapability::ImageInput,
        ),
        (
            json!({"type": "input_audio", "input_audio": {}}),
            LocalCapability::AudioInput,
        ),
        (
            json!({"type": "input_video", "input_video": {}}),
            LocalCapability::VideoInput,
        ),
    ] {
        let request = gateway_request(json!({
            "model": "auto",
            "messages": [{"role": "user", "content": [block]}]
        }));
        assert!(
            request
                .features()
                .contains(&RequestFeature::UnsupportedContent)
        );
        assert!(
            !request
                .features()
                .contains(&RequestFeature::Capability(capability))
        );
    }
}

#[test]
fn destination_model_must_not_be_empty_or_blank() {
    let request = gateway_request(json!({
        "model": "auto",
        "messages": [{"role": "user", "content": "hello"}]
    }));

    for model in ["", "   "] {
        assert!(matches!(
            request.body_bytes_for_model(model),
            Err(GatewayRequestError::Invalid { ref field, .. }) if field == "model"
        ));
    }
}
