//! Buffered Anthropic message response translation.

use super::translate_message_response;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

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

#[test]
fn buffered_thinking_and_tool_use_keep_their_distinct_open_ai_fields() {
    let response = json!({
        "id": "msg-1",
        "type": "message",
        "model": "k3",
        "content": [
            {"type": "thinking", "thinking": "inspect the evidence"},
            {
                "type": "tool_use",
                "id": "call-1",
                "name": "lookup",
                "input": {"city": "Seattle"}
            }
        ],
        "stop_reason": "tool_use"
    });
    let translated =
        translate_message_response(&serde_json::to_vec(&response).expect("JSON"), "fallback")
            .expect("translated response");
    let body: Value = serde_json::from_slice(&translated).expect("translated JSON");
    let message = &body["choices"][0]["message"];

    assert!(message["content"].is_null());
    assert_eq!(message["reasoning_content"], "inspect the evidence");
    assert_eq!(message["tool_calls"][0]["id"], "call-1");
    assert_eq!(message["tool_calls"][0]["function"]["name"], "lookup");
    let arguments: Value = serde_json::from_str(
        message["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("serialized tool arguments"),
    )
    .expect("tool arguments JSON");
    assert_eq!(arguments, json!({"city": "Seattle"}));
}

#[test]
fn buffered_response_created_is_the_current_unix_timestamp() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time after epoch")
        .as_secs();
    let response = json!({
        "type": "message",
        "content": [{"type": "text", "text": "hello"}]
    });
    let translated =
        translate_message_response(&serde_json::to_vec(&response).expect("JSON"), "k3")
            .expect("translated response");
    let body: Value = serde_json::from_slice(&translated).expect("translated JSON");
    let created = body["created"].as_u64().expect("created timestamp");

    assert!(created.abs_diff(now) <= 2, "created={created}, now={now}");
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

/// `refusal` and `pause_turn` are not stops. Reporting them as `stop` tells the
/// client it received a complete answer when it did not.
#[test]
fn stop_reasons_without_an_open_ai_equivalent_are_preserved() {
    for (stop_reason, expected) in [
        ("end_turn", "stop"),
        ("stop_sequence", "stop"),
        ("max_tokens", "length"),
        ("tool_use", "tool_calls"),
        ("refusal", "refusal"),
        ("pause_turn", "pause_turn"),
    ] {
        let response = json!({
            "id": "msg-1",
            "type": "message",
            "role": "assistant",
            "model": "k3",
            "content": [{"type": "text", "text": "hello"}],
            "stop_reason": stop_reason,
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let translated =
            translate_message_response(&serde_json::to_vec(&response).expect("JSON"), "k3")
                .expect("translated response");
        let body: Value = serde_json::from_slice(&translated).expect("translated JSON");
        assert_eq!(body["choices"][0]["finish_reason"], expected);
    }
}
