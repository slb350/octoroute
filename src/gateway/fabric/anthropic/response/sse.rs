//! Incremental Anthropic SSE to OpenAI chat-completion SSE translator.

use super::{
    finish_reason_str, unix_timestamp,
    usage::{AnthropicUsage, read_usage},
};
use crate::gateway::fabric::{
    anthropic::AnthropicAdapterError,
    unknown_types::{self, Adapter, Counters},
};
use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

const MAX_SSE_EVENT_BYTES: usize = 1_048_576; // 1 MiB

/// Incremental Anthropic SSE to OpenAI chat-completion SSE translator.
pub(crate) struct AnthropicSseTranslator {
    buffer: Vec<u8>,
    state: TranslatorState,
}

struct TranslatorState {
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
            state: TranslatorState {
                id: format!("chatcmpl-{}", Uuid::new_v4()),
                model: model.to_string(),
                created: unix_timestamp(),
                prompt_usage: AnthropicUsage::default(),
                tool_indices: BTreeMap::new(),
                skipped_indices: BTreeSet::new(),
                next_tool_index: 0,
                done: false,
            },
        }
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Bytes>, AnthropicAdapterError> {
        self.push_with_counters(bytes, &unknown_types::GLOBAL)
    }

    pub(in crate::gateway::fabric::anthropic) fn push_with_counters(
        &mut self,
        bytes: &[u8],
        counters: &Counters,
    ) -> Result<Vec<Bytes>, AnthropicAdapterError> {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_SSE_EVENT_BYTES {
            return Err(AnthropicAdapterError::Response);
        }
        self.buffer.extend_from_slice(bytes);
        let mut output = Vec::new();
        let mut offset = 0;
        let result = loop {
            let Some((end, delimiter)) = event_boundary(&self.buffer[offset..]) else {
                break Ok(output);
            };
            let event_start = offset;
            let event_end = offset + end;
            offset = event_end + delimiter;
            // `message_stop` already emitted `data: [DONE]`, which terminates
            // an OpenAI stream. A later event cannot be appended without
            // writing past the terminator, so it is skipped rather than
            // translated into a chunk no client will read.
            if self.state.done {
                continue;
            }
            let event = &self.buffer[event_start..event_end];
            match self.state.translate_event(event, counters) {
                Ok(Some(translated)) => output.push(translated),
                Ok(None) => {}
                Err(error) => break Err(error),
            }
        };
        // One compaction per push: O(1) when offset is 0 or buffer.len(), and
        // shifts only the leftover partial event otherwise.
        self.buffer.drain(..offset);
        result
    }

    pub(crate) fn finish(&mut self) -> Result<Vec<Bytes>, AnthropicAdapterError> {
        if !self.buffer.iter().all(u8::is_ascii_whitespace) || !self.state.done {
            return Err(AnthropicAdapterError::Response);
        }
        Ok(Vec::new())
    }
}

#[derive(Serialize)]
struct SseChunk<'a, D: Serialize> {
    id: &'a str,
    object: &'static str,
    created: u64,
    model: &'a str,
    choices: [SseChoice<'a, D>; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Value>,
}

#[derive(Serialize)]
struct SseChoice<'a, D: Serialize> {
    index: u8,
    delta: D,
    finish_reason: Option<&'a str>,
}

#[derive(Serialize)]
struct RoleDelta {
    role: &'static str,
}

#[derive(Serialize)]
struct ContentDelta<'a> {
    content: &'a str,
}

#[derive(Serialize)]
struct ReasoningDelta<'a> {
    reasoning_content: &'a str,
}

#[derive(Serialize)]
struct EmptyDelta {}

#[derive(Serialize)]
struct ToolCallStartDelta<'a> {
    tool_calls: [ToolCallStart<'a>; 1],
}

#[derive(Serialize)]
struct ToolCallStart<'a> {
    index: u64,
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    function: ToolCallStartFunction<'a>,
}

#[derive(Serialize)]
struct ToolCallStartFunction<'a> {
    name: &'a str,
    arguments: &'static str,
}

#[derive(Serialize)]
struct ToolCallArgsDelta<'a> {
    tool_calls: [ToolCallArgs<'a>; 1],
}

#[derive(Serialize)]
struct ToolCallArgs<'a> {
    index: u64,
    function: ToolCallArgsFunction<'a>,
}

#[derive(Serialize)]
struct ToolCallArgsFunction<'a> {
    arguments: &'a str,
}

impl TranslatorState {
    fn translate_event(
        &mut self,
        event: &[u8],
        counters: &Counters,
    ) -> Result<Option<Bytes>, AnthropicAdapterError> {
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
                    if let Some(message_id) = message.get("id").and_then(Value::as_str) {
                        self.id = message_id.to_string();
                    }
                    if let Some(message_model) = message.get("model").and_then(Value::as_str) {
                        self.model = message_model.to_string();
                    }
                    self.prompt_usage = read_usage(message.get("usage")).unwrap_or_default();
                }
                Ok(Some(self.render_chunk(
                    RoleDelta { role: "assistant" },
                    None,
                    None,
                )?))
            }
            "content_block_start" => self.content_block_start(&value, counters),
            "content_block_delta" => self.content_block_delta(&value, counters),
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
                Ok(Some(self.render_chunk(
                    EmptyDelta {},
                    finish_reason_str(stop_reason),
                    usage,
                )?))
            }
            "message_stop" => {
                self.done = true;
                Ok(Some(Bytes::from_static(b"data: [DONE]\n\n")))
            }
            "error" => Err(AnthropicAdapterError::Response),
            _ => {
                counters.record(Adapter::Anthropic);
                Ok(None)
            }
        }
    }

    fn content_block_start(
        &mut self,
        value: &Value,
        counters: &Counters,
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
                    .then(|| self.render_chunk(ContentDelta { content: text }, None, None))
                    .transpose()
            }
            Some("thinking") => {
                let thinking = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                (!thinking.is_empty())
                    .then(|| {
                        self.render_chunk(
                            ReasoningDelta {
                                reasoning_content: thinking,
                            },
                            None,
                            None,
                        )
                    })
                    .transpose()
            }
            Some("tool_use") => {
                let tool_index = self.next_tool_index;
                self.next_tool_index = self.next_tool_index.saturating_add(1);
                self.tool_indices.insert(index, tool_index);
                let tool_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicAdapterError::Response)?;
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicAdapterError::Response)?;
                Ok(Some(self.render_chunk(
                    ToolCallStartDelta {
                        tool_calls: [ToolCallStart {
                            index: tool_index,
                            id: tool_id,
                            kind: "function",
                            function: ToolCallStartFunction {
                                name,
                                arguments: "",
                            },
                        }],
                    },
                    None,
                    None,
                )?))
            }
            _ => {
                // Remember the index: this block's deltas are about to arrive and
                // are just as unrepresentable as the block itself.
                self.skipped_indices.insert(index);
                counters.record(Adapter::Anthropic);
                Ok(None)
            }
        }
    }

    fn content_block_delta(
        &self,
        value: &Value,
        counters: &Counters,
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
            Some("text_delta") => Ok(Some(
                self.render_chunk(
                    ContentDelta {
                        content: delta
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or(AnthropicAdapterError::Response)?,
                    },
                    None,
                    None,
                )?,
            )),
            Some("thinking_delta") => Ok(Some(
                self.render_chunk(
                    ReasoningDelta {
                        reasoning_content: delta
                            .get("thinking")
                            .and_then(Value::as_str)
                            .ok_or(AnthropicAdapterError::Response)?,
                    },
                    None,
                    None,
                )?,
            )),
            Some("input_json_delta") => {
                // Partial tool arguments are the one delta bound to its block:
                // they are meaningless without the `tool_use` start that named
                // the tool. When that start was skipped as unrepresentable, this
                // is skipped for the same reason and counted the same way.
                if self.skipped_indices.contains(&index) {
                    counters.record(Adapter::Anthropic);
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
                Ok(Some(self.render_chunk(
                    ToolCallArgsDelta {
                        tool_calls: [ToolCallArgs {
                            index: tool_index,
                            function: ToolCallArgsFunction { arguments },
                        }],
                    },
                    None,
                    None,
                )?))
            }
            Some("signature_delta") => Ok(None),
            _ => {
                counters.record(Adapter::Anthropic);
                Ok(None)
            }
        }
    }

    fn render_chunk<D: Serialize>(
        &self,
        delta: D,
        finish_reason: Option<&str>,
        usage: Option<Value>,
    ) -> Result<Bytes, AnthropicAdapterError> {
        let chunk = SseChunk {
            id: &self.id,
            object: "chat.completion.chunk",
            created: self.created,
            model: &self.model,
            choices: [SseChoice {
                index: 0,
                delta,
                finish_reason,
            }],
            usage,
        };
        let mut bytes = Vec::with_capacity(192);
        bytes.extend_from_slice(b"data: ");
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
/// `\r\n\r\n` wins over an earlier CRLF boundary. Bounding the CRLF scan to
/// `lf + 4` guarantees any CRLF match begins strictly before `lf`, so a CRLF
/// match always takes precedence over `lf`.
fn event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2));
    let crlf_slice = match lf {
        Some((position, _)) => &buffer[..position.saturating_add(4).min(buffer.len())],
        None => buffer,
    };
    let crlf = crlf_slice
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4));
    crlf.or(lf)
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

#[cfg(test)]
mod tests {
    use super::parse_sse_event;

    #[test]
    fn sse_data_lines_are_joined_with_exactly_one_newline() {
        let event =
            parse_sse_event(b"event: future\ndata: first\ndata: second").expect("valid SSE event");

        assert_eq!(event.name, Some("future"));
        assert_eq!(event.data, b"first\nsecond");
    }
}
