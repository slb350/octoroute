//! Buffered Anthropic message response translation.

use super::translate_message_response;
use serde_json::{Value, json};

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
