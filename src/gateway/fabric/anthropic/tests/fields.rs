//! Fail-closed rejection of request fields without a verified mapping.

use super::{build_request, chat, config, request};
use serde_json::{Value, json};

/// Every OpenAI field without a verified Anthropic mapping is incompatible, so
/// the route falls through instead of answering a silently different request.
///
/// The last entry is the one that matters: a field this adapter has never heard
/// of must fail too. A denylist of known-unmapped fields would accept it and
/// silently drop it, which is the failure direction the contract forbids.
#[test]
fn unmapped_open_ai_fields_fail_closed() {
    let config = config();
    for field in [
        "seed",
        "frequency_penalty",
        "presence_penalty",
        "logit_bias",
        "stream_options",
        "parallel_tool_calls",
        "user",
        "metadata",
        "service_tier",
        "logprobs",
        "audio",
        "a_field_openai_has_not_invented_yet",
    ] {
        let mut body = chat(json!({}));
        body.as_object_mut()
            .expect("object")
            .insert(field.to_string(), json!(1));
        let error = build_request(&config.providers["kimi"], &request(body))
            .expect_err("unmapped field must fail closed");
        assert!(error.is_incompatible(), "{field} must be incompatible");
    }
}

#[test]
fn structured_output_rejects_before_provider_disclosure() {
    let config = config();
    let error = build_request(
        &config.providers["kimi"],
        &request(chat(json!({
            "response_format": {"type": "json_schema", "json_schema": {"name": "answer"}}
        }))),
    )
    .expect_err("unsupported structured output");
    assert!(error.is_incompatible());
}

/// A key nested inside a message, a content block, a tool, or the `reasoning`
/// object changes the request just as much as a top-level one, so the same
/// fail-closed rule applies at every level.
///
/// Dropping any of these silently is the failure the contract names: the caller
/// gets a plausible answer to a request that was quietly altered.
///
/// Every role carries its own allowlist, so every role is exercised here: a
/// case list that only covers `user` leaves the other three guards free to be
/// deleted with the suite still green.
#[test]
fn unknown_nested_keys_fail_closed() {
    let config = config();
    for (label, body) in [
        (
            "system message cache_control",
            json!({"messages": [
                {"role": "system", "content": "be brief", "cache_control": {"type": "ephemeral"}},
                {"role": "user", "content": "go"}
            ]}),
        ),
        (
            "developer message cache_control",
            json!({"messages": [
                {"role": "developer", "content": "be brief", "cache_control": {"type": "ephemeral"}},
                {"role": "user", "content": "go"}
            ]}),
        ),
        (
            "user message cache_control",
            json!({"messages": [{"role": "user", "content": "go", "cache_control": {"type": "ephemeral"}}]}),
        ),
        (
            "assistant message cache_control",
            json!({"messages": [
                {"role": "user", "content": "go"},
                {"role": "assistant", "content": "sure", "cache_control": {"type": "ephemeral"}}
            ]}),
        ),
        (
            "tool message cache_control",
            json!({"messages": [
                {"role": "user", "content": "go"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "f", "arguments": "{}"}
                }]},
                {"role": "tool", "tool_call_id": "call-1", "content": "out",
                    "cache_control": {"type": "ephemeral"}}
            ]}),
        ),
        (
            "content block cache_control",
            json!({"messages": [{"role": "user", "content": [
                {"type": "text", "text": "go", "cache_control": {"type": "ephemeral"}}
            ]}]}),
        ),
        (
            "content block citations",
            json!({"messages": [{"role": "user", "content": [
                {"type": "text", "text": "go", "citations": []}
            ]}]}),
        ),
        (
            "assistant tool_call field",
            json!({"messages": [
                {"role": "user", "content": "go"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "cache_control": {"type": "ephemeral"},
                    "function": {"name": "f", "arguments": "{}"}
                }]}
            ]}),
        ),
        (
            "assistant tool_call function field",
            json!({"messages": [
                {"role": "user", "content": "go"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "f", "arguments": "{}", "strict": true}
                }]}
            ]}),
        ),
        (
            "tool function strict",
            json!({"tools": [{"type": "function", "function": {
                "name": "f", "parameters": {"type": "object"}, "strict": true
            }}]}),
        ),
        (
            "tool object field",
            json!({"tools": [{"type": "function", "cache_control": {"type": "ephemeral"},
                "function": {"name": "f", "parameters": {"type": "object"}}}]}),
        ),
        (
            "tool_choice field",
            json!({
                "tools": [{"type": "function", "function": {"name": "f"}}],
                "tool_choice": {"type": "function", "mode": "strict", "function": {"name": "f"}}
            }),
        ),
        (
            "tool_choice function field",
            json!({
                "tools": [{"type": "function", "function": {"name": "f"}}],
                "tool_choice": {"type": "function", "function": {"name": "f", "arguments": "{}"}}
            }),
        ),
        (
            "reasoning context",
            json!({"reasoning": {"context": "long"}}),
        ),
        ("reasoning mode", json!({"reasoning": {"mode": "auto"}})),
        (
            "reasoning invented key",
            json!({"reasoning": {"effort": "high", "a_key_openrouter_has_not_invented_yet": 1}}),
        ),
    ] {
        let mut request_body = chat(json!({}));
        request_body
            .as_object_mut()
            .expect("object")
            .extend(body.as_object().expect("object").clone());
        let error =
            build_request(&config.providers["kimi"], &request(request_body)).expect_err(label);
        assert!(error.is_incompatible(), "{label} must be incompatible");
    }
}

/// `modalities` is recognized, and constrained to the one value that maps: a
/// text-only request. Anything else asks for output this adapter cannot carry.
#[test]
fn modalities_are_constrained_to_text() {
    let config = config();
    for accepted in [json!(["text"]), Value::Null] {
        build_request(
            &config.providers["kimi"],
            &request(chat(json!({"modalities": accepted.clone()}))),
        )
        .unwrap_or_else(|_| panic!("modalities {accepted} must translate"));
    }
    for rejected in [json!(["audio"]), json!(["text", "audio"]), json!("text")] {
        let error = build_request(
            &config.providers["kimi"],
            &request(chat(json!({"modalities": rejected.clone()}))),
        )
        .expect_err("non-text modalities must fail closed");
        assert!(error.is_incompatible(), "{rejected} must be incompatible");
    }
}
