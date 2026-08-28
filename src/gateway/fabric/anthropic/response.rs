//! Anthropic Messages response and SSE translation back to OpenAI shape.

mod usage;

use self::usage::{AnthropicUsage, read_usage};
use super::{AnthropicAdapterError, ignore_unknown_type};
use bytes::Bytes;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;

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
            _ => ignore_unknown_type(),
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

/// Incremental Anthropic SSE to OpenAI chat-completion SSE translator.
pub(crate) struct AnthropicSseTranslator {
    buffer: Vec<u8>,
    id: String,
    model: String,
    created: u64,
    prompt_usage: AnthropicUsage,
    tool_indices: BTreeMap<u64, u64>,
    /// Block indices whose `content_block_start` this adapter could not
    /// represent and skipped.
    ///
    /// Their deltas still arrive, and they arrive after the response has
    /// committed, so they have to be skipped too rather than failing.
    skipped_indices: BTreeSet<u64>,
    next_tool_index: u64,
    done: bool,
}

impl AnthropicSseTranslator {
    pub(crate) fn new(model: &str) -> Self {
        Self {
            buffer: Vec::new(),
            id: format!("chatcmpl-{}", Uuid::new_v4()),
            model: model.to_string(),
            created: unix_timestamp(),
            prompt_usage: AnthropicUsage::default(),
            tool_indices: BTreeMap::new(),
            skipped_indices: BTreeSet::new(),
            next_tool_index: 0,
            done: false,
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Bytes>, AnthropicAdapterError> {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_SSE_EVENT_BYTES {
            return Err(AnthropicAdapterError::Response);
        }
        self.buffer.extend_from_slice(bytes);
        let mut output = Vec::new();
        while let Some((end, delimiter)) = event_boundary(&self.buffer) {
            let event = self.buffer.drain(..end).collect::<Vec<_>>();
            self.buffer.drain(..delimiter);
            // `message_stop` already emitted `data: [DONE]`, which terminates
            // an OpenAI stream. A later event cannot be appended without
            // writing past the terminator, so it is drained and dropped
            // rather than translated into a chunk no client will read.
            if self.done {
                continue;
            }
            if let Some(translated) = self.translate_event(&event)? {
                output.push(translated);
            }
        }
        Ok(output)
    }

    pub(crate) fn finish(&mut self) -> Result<Vec<Bytes>, AnthropicAdapterError> {
        if !self.buffer.iter().all(u8::is_ascii_whitespace) || !self.done {
            return Err(AnthropicAdapterError::Response);
        }
        Ok(Vec::new())
    }

    fn translate_event(&mut self, event: &[u8]) -> Result<Option<Bytes>, AnthropicAdapterError> {
        let event = parse_sse_event(event)?;
        if event.data.is_empty() {
            return Ok(None);
        }
        let value: Value =
            serde_json::from_slice(&event.data).map_err(|_| AnthropicAdapterError::Response)?;
        // Anthropic repeats the kind in the JSON body, so that is the
        // authoritative field. An Anthropic-compatible endpoint that only
        // labels the `event:` line would otherwise fail a stream this adapter
        // can read, so the line is honored as a fallback.
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .or(event.name)
            .ok_or(AnthropicAdapterError::Response)?;
        match kind {
            "ping" | "content_block_stop" => Ok(None),
            "message_start" => {
                if let Some(message) = value.get("message").and_then(Value::as_object) {
                    if let Some(id) = message.get("id").and_then(Value::as_str) {
                        self.id = id.to_string();
                    }
                    if let Some(model) = message.get("model").and_then(Value::as_str) {
                        self.model = model.to_string();
                    }
                    self.prompt_usage = read_usage(message.get("usage")).unwrap_or_default();
                }
                Ok(Some(self.chunk(
                    json!({"role": "assistant"}),
                    Value::Null,
                    None,
                )?))
            }
            "content_block_start" => self.content_block_start(&value),
            "content_block_delta" => self.content_block_delta(&value),
            "message_delta" => {
                let stop_reason = value
                    .get("delta")
                    .and_then(Value::as_object)
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str);
                // `message_start` carried the prompt-side counts;
                // `message_delta` carries the output tokens. Each half is
                // reported only if some event actually supplied it.
                let usage = read_usage(value.get("usage"))
                    .map(|delta| delta.merge(self.prompt_usage).into_open_ai());
                Ok(Some(self.chunk(
                    json!({}),
                    finish_reason(stop_reason),
                    usage,
                )?))
            }
            "message_stop" => {
                self.done = true;
                Ok(Some(Bytes::from_static(b"data: [DONE]\n\n")))
            }
            "error" => Err(AnthropicAdapterError::Response),
            _ => {
                ignore_unknown_type();
                Ok(None)
            }
        }
    }

    fn content_block_start(
        &mut self,
        value: &Value,
    ) -> Result<Option<Bytes>, AnthropicAdapterError> {
        let index = value
            .get("index")
            .and_then(Value::as_u64)
            .ok_or(AnthropicAdapterError::Response)?;
        let block = value
            .get("content_block")
            .and_then(Value::as_object)
            .ok_or(AnthropicAdapterError::Response)?;
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                (!text.is_empty())
                    .then(|| self.chunk(json!({"content": text}), Value::Null, None))
                    .transpose()
            }
            Some("thinking") => {
                let thinking = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                (!thinking.is_empty())
                    .then(|| self.chunk(json!({"reasoning_content": thinking}), Value::Null, None))
                    .transpose()
            }
            Some("tool_use") => {
                let tool_index = self.next_tool_index;
                self.next_tool_index = self.next_tool_index.saturating_add(1);
                self.tool_indices.insert(index, tool_index);
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicAdapterError::Response)?;
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicAdapterError::Response)?;
                Ok(Some(self.chunk(
                    json!({
                        "tool_calls": [{
                            "index": tool_index,
                            "id": id,
                            "type": "function",
                            "function": {"name": name, "arguments": ""}
                        }]
                    }),
                    Value::Null,
                    None,
                )?))
            }
            _ => {
                // Remember the index: this block's deltas are about to arrive and
                // are just as unrepresentable as the block itself.
                self.skipped_indices.insert(index);
                ignore_unknown_type();
                Ok(None)
            }
        }
    }

    fn content_block_delta(
        &mut self,
        value: &Value,
    ) -> Result<Option<Bytes>, AnthropicAdapterError> {
        let index = value
            .get("index")
            .and_then(Value::as_u64)
            .ok_or(AnthropicAdapterError::Response)?;
        let delta = value
            .get("delta")
            .and_then(Value::as_object)
            .ok_or(AnthropicAdapterError::Response)?;
        match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => Ok(Some(self.chunk(
                json!({"content": delta.get("text").and_then(Value::as_str).ok_or(AnthropicAdapterError::Response)?}),
                Value::Null,
                None,
            )?)),
            Some("thinking_delta") => Ok(Some(self.chunk(
                json!({"reasoning_content": delta.get("thinking").and_then(Value::as_str).ok_or(AnthropicAdapterError::Response)?}),
                Value::Null,
                None,
            )?)),
            Some("input_json_delta") => {
                // Partial tool arguments are the one delta bound to its block:
                // they are meaningless without the `tool_use` start that named
                // the tool. When that start was skipped as unrepresentable, this
                // is skipped for the same reason and counted the same way.
                //
                // Failing instead would truncate a response the client is
                // already reading, because deltas arrive after commitment. A
                // future Anthropic block carrying `input_json_delta` (a
                // server-side tool, say) would turn a complete generation into a
                // broken stream.
                //
                // `text_delta` and `thinking_delta` are deliberately not gated on
                // the index: they carry their own payload and map to assistant
                // content on their own terms, so dropping them would lose real
                // output rather than protect the stream.
                if self.skipped_indices.contains(&index) {
                    ignore_unknown_type();
                    return Ok(None);
                }
                let tool_index = self
                    .tool_indices
                    .get(&index)
                    .copied()
                    .ok_or(AnthropicAdapterError::Response)?;
                let arguments = delta
                    .get("partial_json")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicAdapterError::Response)?;
                Ok(Some(self.chunk(
                    json!({
                        "tool_calls": [{
                            "index": tool_index,
                            "function": {"arguments": arguments}
                        }]
                    }),
                    Value::Null,
                    None,
                )?))
            }
            Some("signature_delta") => Ok(None),
            _ => {
                ignore_unknown_type();
                Ok(None)
            }
        }
    }

    fn chunk(
        &self,
        delta: Value,
        finish_reason: Value,
        usage: Option<Value>,
    ) -> Result<Bytes, AnthropicAdapterError> {
        let mut chunk = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}]
        });
        if let Some(usage) = usage {
            chunk["usage"] = usage;
        }
        let mut bytes = Vec::from(&b"data: "[..]);
        serde_json::to_writer(&mut bytes, &chunk)
            .map_err(|_| AnthropicAdapterError::Serialization)?;
        bytes.extend_from_slice(b"\n\n");
        Ok(Bytes::from(bytes))
    }
}

/// Find the first event boundary, whichever terminator forms it.
///
/// Scanning the whole buffer for `\n\n` before trying `\r\n\r\n` splits a
/// mixed-terminator stream at the wrong offset: the `\n\n` inside a later
/// `\r\n\r\n` wins over an earlier CRLF boundary. Both candidates are computed
/// and the earlier one is returned, with CRLF preferred where they coincide so
/// the `\r` is consumed rather than left to head the next event.
fn event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4));
    match (lf, crlf) {
        (Some(lf), Some(crlf)) if crlf.0 <= lf.0 => Some(crlf),
        (Some(lf), _) => Some(lf),
        (None, crlf) => crlf,
    }
}

/// One reassembled SSE event: its `event:` label and its concatenated `data:`.
struct SseEvent<'a> {
    name: Option<&'a str>,
    data: Vec<u8>,
}

fn parse_sse_event(event: &[u8]) -> Result<SseEvent<'_>, AnthropicAdapterError> {
    let text = std::str::from_utf8(event).map_err(|_| AnthropicAdapterError::Response)?;
    let mut data = Vec::new();
    let mut name = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push(b'\n');
            }
            data.extend_from_slice(value.strip_prefix(' ').unwrap_or(value).as_bytes());
        } else if let Some(value) = line.strip_prefix("event:") {
            name = Some(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    Ok(SseEvent { name, data })
}

/// Map an Anthropic `stop_reason` onto an OpenAI `finish_reason`.
///
/// Values with no OpenAI equivalent - `refusal`, `pause_turn`, and anything
/// added later - are passed through rather than reported as `stop`. A client
/// that sees `stop` assumes a complete answer, which is exactly wrong for a
/// refused or paused turn.
fn finish_reason(reason: Option<&str>) -> Value {
    match reason {
        Some("end_turn" | "stop_sequence") => Value::String("stop".to_string()),
        Some("max_tokens") => Value::String("length".to_string()),
        Some("tool_use") => Value::String("tool_calls".to_string()),
        Some(other) => Value::String(other.to_string()),
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
    use super::{parse_sse_event, truncate_on_char_boundary};

    #[test]
    fn utf8_truncation_preserves_exact_boundaries_and_backs_up_from_split_characters() {
        let value = "A🙂B";

        assert_eq!(truncate_on_char_boundary(value, 5), "A🙂");
        assert_eq!(truncate_on_char_boundary(value, 4), "A");
    }

    #[test]
    fn sse_data_lines_are_joined_with_exactly_one_newline() {
        let event =
            parse_sse_event(b"event: future\ndata: first\ndata: second").expect("valid SSE event");

        assert_eq!(event.name, Some("future"));
        assert_eq!(event.data, b"first\nsecond");
    }
}
