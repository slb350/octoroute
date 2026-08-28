//! Usage accounting and upstream error translation.

use super::{AnthropicSseTranslator, chunk_json, open_ai_error_body, translate_message_response};
use serde_json::{Value, json};

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

#[test]
fn provider_error_messages_truncate_before_a_split_utf8_character() {
    let oversized = format!("{}étail", "a".repeat(2047));
    let upstream = json!({"error": {"message": oversized}});
    let body: Value = serde_json::from_slice(&open_ai_error_body(
        "provider_request_failed",
        &serde_json::to_vec(&upstream).expect("JSON"),
    ))
    .expect("error JSON");
    let message = body["error"]["message"].as_str().expect("error message");

    assert_eq!(message.len(), 2047);
    assert!(message.bytes().all(|byte| byte == b'a'));
}

/// Reporting zeros for an upstream that sent no usage tells a cost-tracking
/// client the request was free.
#[test]
fn absent_usage_is_omitted_rather_than_reported_as_zero() {
    let response = json!({
        "id": "msg-1",
        "type": "message",
        "role": "assistant",
        "model": "k3",
        "content": [{"type": "text", "text": "hello"}],
        "stop_reason": "end_turn"
    });
    let translated =
        translate_message_response(&serde_json::to_vec(&response).expect("JSON"), "k3")
            .expect("translated response");
    let body: Value = serde_json::from_slice(&translated).expect("translated JSON");
    assert!(
        body.get("usage").is_none(),
        "absent upstream usage must not be reported as zero"
    );
}

/// The stream reports its prompt-side counts in `message_start` and its output
/// count in `message_delta`, so the usage a client sees is a merge of the two.
#[test]
fn streaming_usage_merges_the_prompt_and_completion_halves() {
    let mut translator = AnthropicSseTranslator::new("k3");
    translator
        .push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"model\":\"k3\",\"usage\":{\"input_tokens\":7,\"cache_read_input_tokens\":3}}}\n\n")
        .expect("stream");
    let output = translator
        .push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n")
        .expect("stream");
    assert_eq!(output.len(), 1);
    let usage = &chunk_json(&output[0])["usage"];
    assert_eq!(usage["prompt_tokens"], 10, "cached tokens count as prompt");
    assert_eq!(usage["completion_tokens"], 2);
    assert_eq!(usage["total_tokens"], 12);
    assert_eq!(usage["prompt_tokens_details"]["cached_tokens"], 3);
}

/// A `message_delta` with no usage at all reports none, rather than a chunk
/// claiming the completion was free.
#[test]
fn a_message_delta_without_usage_reports_none() {
    let mut translator = AnthropicSseTranslator::new("k3");
    translator
        .push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"usage\":{\"input_tokens\":7}}}\n\n")
        .expect("stream");
    let output = translator
        .push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n")
        .expect("stream");
    assert_eq!(output.len(), 1);
    assert!(chunk_json(&output[0]).get("usage").is_none());
}

/// The contract's "absent usage omits the key" rule is per field: a half the
/// upstream did not report must not come back as a zero, and the cache counts
/// have to survive the translation.
#[test]
fn partial_usage_omits_the_missing_half_and_carries_cache_tokens() {
    let prompt_only = translated_usage(json!({
        "input_tokens": 5,
        "cache_read_input_tokens": 4,
        "cache_creation_input_tokens": 6
    }));
    assert_eq!(prompt_only["prompt_tokens"], 15);
    assert!(
        prompt_only.get("completion_tokens").is_none(),
        "an unreported completion must not be a zero"
    );
    assert!(prompt_only.get("total_tokens").is_none());
    assert_eq!(prompt_only["prompt_tokens_details"]["cached_tokens"], 4);
    assert_eq!(
        prompt_only["prompt_tokens_details"]["cache_creation_tokens"],
        6
    );

    let completion_only = translated_usage(json!({"output_tokens": 3}));
    assert_eq!(completion_only["completion_tokens"], 3);
    assert!(
        completion_only.get("prompt_tokens").is_none(),
        "an unreported prompt must not be a zero"
    );
    assert!(completion_only.get("total_tokens").is_none());
}

/// A prompt Anthropic reported only as cache counts is still a reported
/// prompt. A fully cached turn can arrive with no `input_tokens` at all, and
/// treating that as "nothing reported" hides the entire prompt cost.
#[test]
fn a_prompt_reported_only_as_cache_counts_is_still_reported() {
    let cache_read_only = translated_usage(json!({"cache_read_input_tokens": 4}));
    assert_eq!(cache_read_only["prompt_tokens"], 4);
    assert_eq!(cache_read_only["prompt_tokens_details"]["cached_tokens"], 4);
    assert!(cache_read_only.get("completion_tokens").is_none());
    assert!(cache_read_only.get("total_tokens").is_none());

    let cache_creation_only = translated_usage(json!({"cache_creation_input_tokens": 6}));
    assert_eq!(cache_creation_only["prompt_tokens"], 6);
    assert_eq!(
        cache_creation_only["prompt_tokens_details"]["cache_creation_tokens"],
        6
    );
}

/// A usage object carrying no counts reports exactly what a missing one does.
/// Anything else would let an empty upstream report stand in for a real one.
#[test]
fn a_usage_object_with_no_counts_is_treated_as_no_usage() {
    let mut translator = AnthropicSseTranslator::new("k3");
    translator
        .push(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"usage\":{\"input_tokens\":7}}}\n\n")
        .expect("stream");
    let output = translator
        .push(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{}}\n\n")
        .expect("stream");
    assert_eq!(output.len(), 1);
    assert!(chunk_json(&output[0]).get("usage").is_none());
}

/// Translate a buffered response carrying `usage`, returning its translated
/// usage object.
fn translated_usage(usage: Value) -> Value {
    let response = json!({
        "id": "msg-1",
        "type": "message",
        "role": "assistant",
        "model": "k3",
        "content": [{"type": "text", "text": "hello"}],
        "stop_reason": "end_turn",
        "usage": usage
    });
    let translated =
        translate_message_response(&serde_json::to_vec(&response).expect("JSON"), "k3")
            .expect("translated response");
    serde_json::from_slice::<Value>(&translated).expect("translated JSON")["usage"].clone()
}
