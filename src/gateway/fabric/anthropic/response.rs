//! Anthropic Messages response and SSE translation back to OpenAI shape.

mod sse;
mod usage;

pub(crate) use self::sse::AnthropicSseTranslator;

use self::usage::{AnthropicUsage, read_usage};
use super::AnthropicAdapterError;
use crate::gateway::fabric::unknown_types::{self, Adapter};
use bytes::Bytes;
use serde_json::{Map, Value, json};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub(crate) fn translate_message_response(
    input: &[u8],
    fallback_model: &str,
) -> Result<Bytes, AnthropicAdapterError> {
    let value: Value =
        serde_json::from_slice(input).map_err(|_| AnthropicAdapterError::Response)?;
    let message = value.as_object().ok_or(AnthropicAdapterError::Response)?;
    if message.get("type").and_then(Value::as_str) != Some("message") {
        return Err(AnthropicAdapterError::Response);
    }
    let id = message
        .get("id")
        .and_then(Value::as_str)
        .map_or_else(|| format!("chatcmpl-{}", Uuid::new_v4()), str::to_string);
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(fallback_model);
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .ok_or(AnthropicAdapterError::Response)?;
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for block in content {
        let block = block.as_object().ok_or(AnthropicAdapterError::Response)?;
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text.push_str(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicAdapterError::Response)?,
            ),
            Some("thinking") => reasoning.push_str(
                block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicAdapterError::Response)?,
            ),
            Some("tool_use") => {
                let input = block.get("input").ok_or(AnthropicAdapterError::Response)?;
                let arguments = serde_json::to_string(input)
                    .map_err(|_| AnthropicAdapterError::Serialization)?;
                tool_calls.push(json!({
                    "id": block.get("id").and_then(Value::as_str).ok_or(AnthropicAdapterError::Response)?,
                    "type": "function",
                    "function": {
                        "name": block.get("name").and_then(Value::as_str).ok_or(AnthropicAdapterError::Response)?,
                        "arguments": arguments
                    }
                }));
            }
            // `redacted_thinking` and any block type added after this release
            // carry no OpenAI equivalent; skipping them keeps the response
            // usable instead of failing a completed generation.
            _ => unknown_types::GLOBAL.record(Adapter::Anthropic),
        }
    }
    let mut assistant = Map::from_iter([
        ("role".to_string(), Value::String("assistant".to_string())),
        (
            "content".to_string(),
            if text.is_empty() {
                Value::Null
            } else {
                Value::String(text)
            },
        ),
    ]);
    if !reasoning.is_empty() {
        assistant.insert("reasoning_content".to_string(), Value::String(reasoning));
    }
    if !tool_calls.is_empty() {
        assistant.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    let mut response = json!({
        "id": id,
        "object": "chat.completion",
        "created": unix_timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": Value::Object(assistant),
            "finish_reason": finish_reason(message.get("stop_reason").and_then(Value::as_str))
        }],
    });
    if let Some(usage) = read_usage(message.get("usage")).map(AnthropicUsage::into_open_ai) {
        response["usage"] = usage;
    }
    serde_json::to_vec(&response)
        .map(Bytes::from)
        .map_err(|_| AnthropicAdapterError::Serialization)
}

/// Longest upstream error message Octoroute will relay to a client.
const MAX_ERROR_MESSAGE_BYTES: usize = 2048;

/// Translate an Anthropic error body into OpenAI shape, preserving its diagnosis.
///
/// The upstream message and type are what distinguish a context-length overflow
/// from a credit-balance failure from a missing model, so they are carried
/// through rather than replaced. `code` remains Octoroute's own bounded
/// classification of the HTTP status.
pub(crate) fn open_ai_error_body(code: &str, upstream: &[u8]) -> Bytes {
    let parsed = serde_json::from_slice::<Value>(upstream).ok();
    let error = parsed
        .as_ref()
        .and_then(|body| body.get("error"))
        .and_then(Value::as_object);
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .map_or_else(
            || "provider returned an error response".to_string(),
            |message| truncate_on_char_boundary(message, MAX_ERROR_MESSAGE_BYTES),
        );
    let upstream_type = error
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
        .map(|kind| truncate_on_char_boundary(kind, MAX_ERROR_MESSAGE_BYTES));
    let mut body = Map::from_iter([
        ("message".to_string(), Value::String(message)),
        (
            "type".to_string(),
            Value::String("upstream_error".to_string()),
        ),
        ("code".to_string(), Value::String(code.to_string())),
    ]);
    if let Some(upstream_type) = upstream_type {
        body.insert("upstream_type".to_string(), Value::String(upstream_type));
    }
    serde_json::to_vec(&json!({"error": Value::Object(body)}))
        .map(Bytes::from)
        .expect("the bounded provider error body serializes")
}

/// Truncate to at most `limit` bytes without splitting a UTF-8 character.
fn truncate_on_char_boundary(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    // Byte index zero is always a character boundary, so this cannot underflow.
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

/// Map an Anthropic `stop_reason` onto an OpenAI `finish_reason`.
///
/// Values with no OpenAI equivalent - `refusal`, `pause_turn`, and anything
/// added later - are passed through rather than reported as `stop`. A client
/// that sees `stop` assumes a complete answer, which is exactly wrong for a
/// refused or paused turn.
fn finish_reason_str(reason: Option<&str>) -> Option<&str> {
    match reason {
        Some("end_turn" | "stop_sequence") => Some("stop"),
        Some("max_tokens") => Some("length"),
        Some("tool_use") => Some("tool_calls"),
        Some(other) => Some(other),
        None => None,
    }
}

fn finish_reason(reason: Option<&str>) -> Value {
    match finish_reason_str(reason) {
        Some(value) => Value::String(value.to_string()),
        None => Value::Null,
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::truncate_on_char_boundary;

    #[test]
    fn utf8_truncation_preserves_exact_boundaries_and_backs_up_from_split_characters() {
        let value = "A🙂B";

        assert_eq!(truncate_on_char_boundary(value, 5), "A🙂");
        assert_eq!(truncate_on_char_boundary(value, 4), "A");
    }
}
