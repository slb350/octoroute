//! Anthropic adapter translation tests.

use super::request::build_request;
use super::response::{AnthropicSseTranslator, open_ai_error_body, translate_message_response};
use crate::gateway::{fabric::FabricConfig, request::GatewayRequest};
use serde_json::{Value, json};

const REPOSITORY_CONFIG: &str = include_str!("../../../../config.toml");

fn config() -> FabricConfig {
    FabricConfig::from_toml(REPOSITORY_CONFIG).expect("repository config")
}

fn request(value: Value) -> GatewayRequest {
    GatewayRequest::parse(&serde_json::to_vec(&value).expect("request JSON"))
        .expect("gateway request")
}

/// Build against the shipped `kimi` provider, returning the translated body.
fn translate(value: Value) -> Value {
    let config = config();
    let translated =
        build_request(&config.providers["kimi"], &request(value)).expect("Anthropic request");
    serde_json::from_slice(&translated.body).expect("translated JSON")
}

fn chat(extra: Value) -> Value {
    let mut body = json!({
        "model": "cloud-sota",
        "messages": [{"role": "user", "content": "answer"}]
    });
    body.as_object_mut()
        .expect("object")
        .extend(extra.as_object().expect("object").clone());
    body
}

#[test]
fn open_ai_messages_tools_and_reasoning_translate_explicitly() {
    let body = translate(json!({
        "model": "cloud-sota",
        "messages": [
            {"role": "system", "content": "Be concise."},
            {"role": "user", "content": "Read the file."},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{\"path\":\"README.md\"}"}
                }]
            },
            {"role": "tool", "tool_call_id": "call-1", "content": "contents"}
        ],
        "stream": true,
        "max_completion_tokens": 8192,
        "reasoning_effort": "high",
        "tools": [{
            "type": "function",
            "function": {
                "name": "read_file",
                "parameters": {"type": "object", "properties": {}}
            }
        }]
    }));

    assert_eq!(body["model"], "k3");
    assert_eq!(body["max_tokens"], 8192);
    assert_eq!(body["system"][0]["text"], "Be concise.");
    assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
    assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
}

/// `max_tokens` is the total for thinking plus the answer, so the budget has to
/// leave a usable answer allowance rather than consuming all but one token.
#[test]
fn thinking_budget_reserves_an_answer_allowance() {
    for (max_tokens, effort) in [
        (2_048_u64, "high"),
        (4_096, "high"),
        (8_192, "high"),
        (16_384, "high"),
        (4_096, "medium"),
        (32_768, "xhigh"),
    ] {
        let body = translate(chat(
            json!({"max_tokens": max_tokens, "reasoning_effort": effort}),
        ));
        let budget = body["thinking"]["budget_tokens"]
            .as_u64()
            .expect("thinking budget");
        assert!(
            budget >= 1_024,
            "budget {budget} is below Anthropic's minimum for max_tokens {max_tokens}"
        );
        assert!(
            max_tokens - budget >= budget,
            "budget {budget} leaves only {} answer tokens of {max_tokens}",
            max_tokens - budget
        );
    }
}

/// Below twice Anthropic's minimum budget no thinking allocation both clears the
/// minimum and leaves an answer, so the request goes out without thinking.
#[test]
fn unaffordable_thinking_budget_is_omitted_rather_than_starved() {
    for max_tokens in [1_024_u64, 1_025, 2_047] {
        let body = translate(chat(
            json!({"max_tokens": max_tokens, "reasoning_effort": "high"}),
        ));
        assert!(
            body.get("thinking").is_none(),
            "max_tokens {max_tokens} must not carry a thinking budget"
        );
    }
}

/// Thinking is opt-in. Neither the shipped `kimi` provider nor the route default
/// asks for it, so an ordinary request must not arrive with thinking enabled.
#[test]
fn thinking_is_not_enabled_without_an_explicit_reasoning_control() {
    let body = translate(chat(json!({"max_tokens": 200_000})));
    assert!(body.get("thinking").is_none());
}

/// Anthropic rejects `temperature`, `top_p`, and `top_k` alongside thinking.
#[test]
fn sampling_controls_are_dropped_when_thinking_is_enabled() {
    let body = translate(chat(json!({
        "max_tokens": 8_192,
        "reasoning_effort": "high",
        "temperature": 0.7,
        "top_p": 0.9,
        "top_k": 40
    })));
    assert!(body.get("thinking").is_some());
    for field in ["temperature", "top_p", "top_k"] {
        assert!(
            body.get(field).is_none(),
            "{field} must not accompany enabled thinking"
        );
    }
}

#[test]
fn caller_sampling_survives_when_thinking_is_disabled() {
    let body = translate(chat(json!({"temperature": 0.7, "top_p": 0.9})));
    assert!(body.get("thinking").is_none());
    assert_eq!(body["temperature"], 0.7);
    assert_eq!(body["top_p"], 0.9);
}

#[test]
fn malformed_controls_fail_closed() {
    let config = config();
    for malformed in [
        json!({"n": "1"}),
        json!({"response_format": "text"}),
        json!({"max_completion_tokens": "many"}),
        json!({"reasoning_effort": 1}),
    ] {
        let error = build_request(&config.providers["kimi"], &request(chat(malformed)))
            .expect_err("malformed control must fail closed");
        assert!(error.is_incompatible());
    }
}

/// Anthropic carries system text in a dedicated field, so a mid-conversation
/// instruction cannot be represented in place and must not be hoisted.
#[test]
fn system_message_after_conversation_content_fails_closed() {
    let config = config();
    let error = build_request(
        &config.providers["kimi"],
        &request(json!({
            "model": "cloud-sota",
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "system", "content": "LATE"},
                {"role": "user", "content": "second"}
            ]
        })),
    )
    .expect_err("a late system message must fail closed");
    assert!(error.is_incompatible());
}

#[test]
fn leading_system_messages_are_still_accepted() {
    let body = translate(json!({
        "model": "cloud-sota",
        "messages": [
            {"role": "system", "content": "one"},
            {"role": "developer", "content": "two"},
            {"role": "user", "content": "answer"}
        ]
    }));
    assert_eq!(body["system"][0]["text"], "one");
    assert_eq!(body["system"][1]["text"], "two");
    assert_eq!(body["messages"].as_array().expect("messages").len(), 1);
}

/// Prior `tool_use`/`tool_result` blocks are only valid alongside the definitions
/// they name, so `tool_choice: "none"` must keep the tools array.
#[test]
fn tool_choice_none_keeps_the_tools_array() {
    let body = translate(json!({
        "model": "cloud-sota",
        "messages": [
            {"role": "user", "content": "go"},
            {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{}"}
                }]
            },
            {"role": "tool", "tool_call_id": "call-1", "content": "contents"}
        ],
        "tool_choice": "none",
        "tools": [{
            "type": "function",
            "function": {"name": "read_file", "parameters": {"type": "object"}}
        }]
    }));
    assert_eq!(body["tool_choice"]["type"], "none");
    assert_eq!(body["tools"][0]["name"], "read_file");
}

/// Every OpenAI field without a verified Anthropic mapping is incompatible, so
/// the route falls through instead of answering a silently different request.
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

#[test]
fn anthropic_message_response_becomes_open_ai_chat_completion() {
    let response = json!({
        "id": "msg-1",
        "type": "message",
        "role": "assistant",
        "model": "k3",
        "content": [{"type": "text", "text": "hello"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 11, "output_tokens": 3}
    });
    let translated =
        translate_message_response(&serde_json::to_vec(&response).expect("JSON"), "k3")
            .expect("translated response");
    let body: Value = serde_json::from_slice(&translated).expect("translated JSON");
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    assert_eq!(body["choices"][0]["finish_reason"], "stop");
    assert_eq!(body["usage"]["prompt_tokens"], 11);
}

/// A block type Octoroute does not know is skipped, not fatal: a completed
/// generation must not be discarded because it carried `redacted_thinking`.
#[test]
fn unknown_response_block_types_are_skipped() {
    let response = json!({
        "id": "msg-1",
        "type": "message",
        "role": "assistant",
        "model": "k3",
        "content": [
            {"type": "redacted_thinking", "data": "opaque"},
            {"type": "text", "text": "hello"}
        ],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
    });
    let translated =
        translate_message_response(&serde_json::to_vec(&response).expect("JSON"), "k3")
            .expect("unknown blocks must not fail a completed generation");
    let body: Value = serde_json::from_slice(&translated).expect("translated JSON");
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
}

#[test]
fn fragmented_anthropic_sse_is_incrementally_translated() {
    let mut translator = AnthropicSseTranslator::new("k3");
    let mut output = Vec::new();
    for event in [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"model\":\"k3\",\"usage\":{\"input_tokens\":7}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ] {
        output.extend(translator.push(event.as_bytes()).expect("translated chunk"));
    }
    translator.finish().expect("complete stream");
    let rendered = output
        .iter()
        .map(|chunk| String::from_utf8_lossy(chunk).to_string())
        .collect::<String>();
    assert!(rendered.contains("\"content\":\"hi\""));
    assert!(rendered.contains("data: [DONE]"));
}

/// An event type added after this release must not truncate a committed stream.
#[test]
fn unknown_sse_event_types_are_skipped_without_truncating_the_stream() {
    let mut translator = AnthropicSseTranslator::new("k3");
    let mut output = Vec::new();
    for event in [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"model\":\"k3\",\"usage\":{\"input_tokens\":7}}}\n\n",
        "event: future_event\ndata: {\"type\":\"future_event\",\"detail\":\"unknown\"}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"future_delta\",\"value\":1}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ] {
        output.extend(translator.push(event.as_bytes()).expect("translated chunk"));
    }
    translator.finish().expect("complete stream");
    let rendered = output
        .iter()
        .map(|chunk| String::from_utf8_lossy(chunk).to_string())
        .collect::<String>();
    assert!(rendered.contains("\"content\":\"hi\""));
    assert!(rendered.contains("data: [DONE]"));
}

/// An `error` event stays fatal even though unknown events are skipped.
#[test]
fn anthropic_error_events_remain_fatal() {
    let mut translator = AnthropicSseTranslator::new("k3");
    translator
        .push(b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}\n\n")
        .expect_err("an error event must fail the stream");
}

/// The upstream diagnosis is what distinguishes a context overflow from a
/// credit-balance failure, so it is carried through rather than replaced.
#[test]
fn provider_error_bodies_preserve_the_upstream_diagnosis() {
    let upstream = json!({
        "type": "error",
        "error": {"type": "invalid_request_error", "message": "prompt is too long: 300000 tokens"}
    });
    let body: Value = serde_json::from_slice(&open_ai_error_body(
        "provider_request_failed",
        &serde_json::to_vec(&upstream).expect("JSON"),
    ))
    .expect("error JSON");
    assert_eq!(
        body["error"]["message"],
        "prompt is too long: 300000 tokens"
    );
    assert_eq!(body["error"]["upstream_type"], "invalid_request_error");
    assert_eq!(body["error"]["code"], "provider_request_failed");
    assert_eq!(body["error"]["type"], "upstream_error");
}

#[test]
fn unparseable_provider_error_bodies_fall_back_to_a_fixed_message() {
    let body: Value = serde_json::from_slice(&open_ai_error_body(
        "provider_server_error",
        b"<html>502</html>",
    ))
    .expect("error JSON");
    assert_eq!(
        body["error"]["message"],
        "provider returned an error response"
    );
    assert_eq!(body["error"]["code"], "provider_server_error");
    assert!(body["error"].get("upstream_type").is_none());
}
