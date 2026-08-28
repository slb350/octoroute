//! Codex CLI event-stream parsing and OpenAI response rendering.

use super::{CodexAdapterError, EVENT_LINE_MAX_BYTES};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub(super) fn parse_events(
    input: &[u8],
) -> Result<(CodexReply, Option<CodexUsage>), CodexAdapterError> {
    let mut final_message: Option<Cow<'_, str>> = None;
    let mut usage: Option<CodexUsage> = None;
    let mut completed = false;
    for line in input.split(|byte| *byte == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if line.len() > EVENT_LINE_MAX_BYTES {
            return Err(CodexAdapterError::Contract);
        }
        let event: Event<'_> =
            serde_json::from_slice(line).map_err(|_| CodexAdapterError::Contract)?;
        let kind = event.kind.ok_or(CodexAdapterError::Contract)?;
        if completed {
            return Err(CodexAdapterError::Contract);
        }
        match kind.as_ref() {
            "thread.started" | "turn.started" => {}
            "item.started" | "item.updated" | "item.completed" => {
                let item = event.item.ok_or(CodexAdapterError::Contract)?;
                match item.kind.as_deref() {
                    Some("agent_message") if kind != "item.completed" => {}
                    Some("agent_message") => {
                        if final_message.is_some() {
                            return Err(CodexAdapterError::Contract);
                        }
                        final_message = Some(item.text.ok_or(CodexAdapterError::Contract)?);
                    }
                    // Item types Octoroute does not consume, including any the
                    // CLI adds later. Failing here would turn every request into
                    // a 502 on the next Codex release.
                    _ => ignore_unknown_event(),
                }
            }
            "turn.completed" if final_message.is_some() => {
                usage = event.usage.map(CodexUsage::from);
                completed = true;
            }
            "error" | "turn.failed" => return Err(CodexAdapterError::Process),
            // As above: an unrecognized event type is skipped, not fatal. The
            // `turn.completed` requirement below still rejects a truncated run.
            _ => ignore_unknown_event(),
        }
    }
    if !completed {
        return Err(CodexAdapterError::Contract);
    }
    let reply: CodexReply =
        serde_json::from_str(final_message.ok_or(CodexAdapterError::Contract)?.as_ref())
            .map_err(|_| CodexAdapterError::Contract)?;
    reply.validate()?;
    Ok((reply, usage))
}

/// Count of Codex CLI event and item types Octoroute skipped as unrecognized.
static IGNORED_CODEX_EVENTS: AtomicU64 = AtomicU64::new(0);

fn ignore_unknown_event() {
    IGNORED_CODEX_EVENTS.fetch_add(1, Ordering::Relaxed);
}

/// Read the skipped-event counter for the Prometheus registry.
pub(crate) fn ignored_unknown_events() -> u64 {
    IGNORED_CODEX_EVENTS.load(Ordering::Relaxed)
}

/// Token accounting reported by `turn.completed`.
///
/// Cost-tracking clients read `usage`, and omitting it makes Codex traffic
/// invisible to them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodexUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

impl From<RawCodexUsage> for CodexUsage {
    fn from(usage: RawCodexUsage) -> Self {
        Self {
            prompt_tokens: usage.input_tokens.unwrap_or_default(),
            completion_tokens: usage.output_tokens.unwrap_or_default(),
        }
    }
}

impl CodexUsage {
    fn to_value(self) -> Value {
        json!({
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.prompt_tokens.saturating_add(self.completion_tokens)
        })
    }
}

#[derive(Deserialize)]
struct Event<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    item: Option<EventItem<'a>>,
    usage: Option<RawCodexUsage>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct RawCodexUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct EventItem<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
pub(super) struct CodexReply {
    pub(super) content: Option<String>,
    pub(super) reasoning_content: Option<String>,
    pub(super) tool_calls: Vec<CodexToolCall>,
    pub(super) finish_reason: String,
}

impl CodexReply {
    fn validate(&self) -> Result<(), CodexAdapterError> {
        if !matches!(
            self.finish_reason.as_str(),
            "stop" | "tool_calls" | "length"
        ) || (self.finish_reason == "tool_calls") != !self.tool_calls.is_empty()
            // An empty string is not an answer. Accepting it turns a Codex run
            // that produced nothing into a successful, empty completion.
            || (self.content.as_deref().is_none_or(str::is_empty) && self.tool_calls.is_empty())
        {
            return Err(CodexAdapterError::Contract);
        }
        for call in &self.tool_calls {
            if !safe_identifier(&call.id)
                || !safe_identifier(&call.name)
                || serde_json::from_str::<Value>(&call.arguments).is_err()
            {
                return Err(CodexAdapterError::Contract);
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub(super) struct CodexToolCall {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) arguments: String,
}

pub(super) fn render_open_ai_reply(
    model: &str,
    stream: bool,
    reply: CodexReply,
    usage: Option<CodexUsage>,
) -> Result<Bytes, CodexAdapterError> {
    let id = format!("chatcmpl-{}", Uuid::new_v4());
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let stream_tool_calls = reply
        .tool_calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            json!({
                "index": index,
                "id": call.id,
                "type": "function",
                "function": {"name": call.name, "arguments": call.arguments}
            })
        })
        .collect::<Vec<_>>();
    let value = if stream {
        let mut delta = json!({"role": "assistant"});
        if let Some(content) = reply.content {
            delta["content"] = Value::String(content);
        }
        if let Some(reasoning) = reply.reasoning_content {
            delta["reasoning_content"] = Value::String(reasoning);
        }
        if !stream_tool_calls.is_empty() {
            delta["tool_calls"] = Value::Array(stream_tool_calls);
        }
        let mut chunk = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": reply.finish_reason
            }]
        });
        if let Some(usage) = usage {
            chunk["usage"] = usage.to_value();
        }
        let mut bytes = Vec::from(&b"data: "[..]);
        serde_json::to_writer(&mut bytes, &chunk).map_err(|_| CodexAdapterError::Contract)?;
        bytes.extend_from_slice(b"\n\ndata: [DONE]\n\n");
        return Ok(Bytes::from(bytes));
    } else {
        let mut message = json!({"role": "assistant", "content": reply.content});
        if let Some(reasoning) = reply.reasoning_content {
            message["reasoning_content"] = Value::String(reasoning);
        }
        if !reply.tool_calls.is_empty() {
            message["tool_calls"] = Value::Array(
                reply
                    .tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {"name": call.name, "arguments": call.arguments}
                        })
                    })
                    .collect(),
            );
        }
        let mut completion = json!({
            "id": id,
            "object": "chat.completion",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": reply.finish_reason
            }]
        });
        if let Some(usage) = usage {
            completion["usage"] = usage.to_value();
        }
        completion
    };
    serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|_| CodexAdapterError::Contract)
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}
