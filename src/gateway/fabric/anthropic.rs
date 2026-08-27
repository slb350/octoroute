//! Translation between OpenAI chat completions and Anthropic Messages.

use super::{ProviderConfig, ReasoningEffort};
use crate::gateway::request::GatewayRequest;
use bytes::Bytes;
use serde_json::{Map, Number, Value, json};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use uuid::Uuid;

const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub(super) struct AnthropicRequest {
    pub(super) body: Bytes,
}

pub(super) fn build_request(
    config: &ProviderConfig,
    request: &GatewayRequest,
    route_effort: ReasoningEffort,
) -> Result<AnthropicRequest, AnthropicAdapterError> {
    let source = request.body_value_for_model(&config.model)?;
    let source = source
        .as_object()
        .expect("gateway requests are validated objects");
    reject_unsupported_request_fields(source)?;

    let mut system = Vec::new();
    let mut messages = Vec::new();
    let source_messages = source
        .get("messages")
        .and_then(Value::as_array)
        .ok_or(AnthropicAdapterError::Incompatible("messages"))?;
    for message in source_messages {
        translate_message(message, &mut system, &mut messages)?;
    }
    if messages.is_empty() {
        return Err(AnthropicAdapterError::Incompatible("messages"));
    }

    let max_tokens = requested_max_tokens(source)
        .or(config.max_tokens)
        .ok_or(AnthropicAdapterError::Incompatible("max_tokens"))?;
    let stream = source
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut body = Map::from_iter([
        ("model".to_string(), Value::String(config.model.clone())),
        ("messages".to_string(), Value::Array(messages)),
        (
            "max_tokens".to_string(),
            Value::Number(Number::from(max_tokens)),
        ),
        ("stream".to_string(), Value::Bool(stream)),
    ]);
    if !system.is_empty() {
        body.insert("system".to_string(), Value::Array(system));
    }
    copy_number(source, &mut body, "top_p")?;
    copy_number(source, &mut body, "top_k")?;
    copy_stop(source, &mut body)?;
    if let Some(temperature) = config.temperature {
        body.insert(
            "temperature".to_string(),
            Value::Number(
                Number::from_f64(temperature).ok_or(AnthropicAdapterError::Serialization)?,
            ),
        );
    }
    translate_tools(source, &mut body)?;
    let effort = requested_reasoning_effort(source)
        .or(config.reasoning_effort)
        .unwrap_or(route_effort);
    if let Some(budget) = thinking_budget(effort, max_tokens) {
        body.insert(
            "thinking".to_string(),
            json!({"type": "enabled", "budget_tokens": budget}),
        );
    }

    serde_json::to_vec(&Value::Object(body))
        .map(Bytes::from)
        .map(|body| AnthropicRequest { body })
        .map_err(|_| AnthropicAdapterError::Serialization)
}

fn reject_unsupported_request_fields(
    source: &Map<String, Value>,
) -> Result<(), AnthropicAdapterError> {
    if source
        .get("n")
        .and_then(Value::as_u64)
        .is_some_and(|value| value != 1)
        || source
            .get("modalities")
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().any(|value| value.as_str() != Some("text")))
        || source
            .get("response_format")
            .and_then(Value::as_object)
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|value| value != "text")
        || ["logprobs", "top_logprobs", "audio"]
            .iter()
            .any(|field| source.get(*field).is_some_and(|value| !value.is_null()))
    {
        return Err(AnthropicAdapterError::Incompatible("request feature"));
    }
    Ok(())
}

fn translate_message(
    message: &Value,
    system: &mut Vec<Value>,
    messages: &mut Vec<Value>,
) -> Result<(), AnthropicAdapterError> {
    let message = message
        .as_object()
        .ok_or(AnthropicAdapterError::Incompatible("message"))?;
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .ok_or(AnthropicAdapterError::Incompatible("message role"))?;
    match role {
        "system" | "developer" => {
            system.extend(text_blocks(message.get("content"))?);
        }
        "user" => push_message(messages, "user", content_blocks(message.get("content"))?),
        "assistant" => {
            let mut blocks = match message.get("content") {
                None | Some(Value::Null) => Vec::new(),
                content => content_blocks(content)?,
            };
            if let Some(tool_calls) = message.get("tool_calls") {
                let tool_calls = tool_calls
                    .as_array()
                    .ok_or(AnthropicAdapterError::Incompatible("tool_calls"))?;
                for call in tool_calls {
                    let call = call
                        .as_object()
                        .ok_or(AnthropicAdapterError::Incompatible("tool call"))?;
                    let function = call
                        .get("function")
                        .and_then(Value::as_object)
                        .ok_or(AnthropicAdapterError::Incompatible("tool call"))?;
                    let id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or(AnthropicAdapterError::Incompatible("tool call id"))?;
                    let name = function
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or(AnthropicAdapterError::Incompatible("tool call name"))?;
                    let arguments = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .ok_or(AnthropicAdapterError::Incompatible("tool call arguments"))?;
                    let input = serde_json::from_str::<Value>(arguments)
                        .map_err(|_| AnthropicAdapterError::Incompatible("tool call arguments"))?;
                    if !input.is_object() {
                        return Err(AnthropicAdapterError::Incompatible("tool call arguments"));
                    }
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input
                    }));
                }
            }
            if blocks.is_empty() {
                return Err(AnthropicAdapterError::Incompatible("assistant message"));
            }
            push_message(messages, "assistant", blocks);
        }
        "tool" => {
            let id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .ok_or(AnthropicAdapterError::Incompatible("tool_call_id"))?;
            let content = tool_result_content(message.get("content"))?;
            push_message(
                messages,
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": content
                })],
            );
        }
        _ => return Err(AnthropicAdapterError::Incompatible("message role")),
    }
    Ok(())
}

fn text_blocks(content: Option<&Value>) -> Result<Vec<Value>, AnthropicAdapterError> {
    let blocks = content_blocks(content)?;
    if blocks
        .iter()
        .any(|block| block.get("type").and_then(Value::as_str) != Some("text"))
    {
        return Err(AnthropicAdapterError::Incompatible("system content"));
    }
    Ok(blocks)
}

fn content_blocks(content: Option<&Value>) -> Result<Vec<Value>, AnthropicAdapterError> {
    match content {
        Some(Value::String(text)) => Ok(vec![json!({"type": "text", "text": text})]),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|block| {
                let block = block
                    .as_object()
                    .ok_or(AnthropicAdapterError::Incompatible("content block"))?;
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    return Err(AnthropicAdapterError::Incompatible("content block"));
                }
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicAdapterError::Incompatible("text content"))?;
                Ok(json!({"type": "text", "text": text}))
            })
            .collect(),
        _ => Err(AnthropicAdapterError::Incompatible("message content")),
    }
}

fn tool_result_content(content: Option<&Value>) -> Result<Value, AnthropicAdapterError> {
    match content {
        Some(Value::String(text)) => Ok(Value::String(text.clone())),
        Some(Value::Array(_)) => Ok(Value::Array(content_blocks(content)?)),
        _ => Err(AnthropicAdapterError::Incompatible("tool result")),
    }
}

fn push_message(messages: &mut Vec<Value>, role: &str, mut blocks: Vec<Value>) {
    if let Some(previous) = messages.last_mut().and_then(Value::as_object_mut)
        && previous.get("role").and_then(Value::as_str) == Some(role)
        && let Some(content) = previous.get_mut("content").and_then(Value::as_array_mut)
    {
        content.append(&mut blocks);
        return;
    }
    messages.push(json!({"role": role, "content": blocks}));
}

fn translate_tools(
    source: &Map<String, Value>,
    destination: &mut Map<String, Value>,
) -> Result<(), AnthropicAdapterError> {
    let Some(tools) = source.get("tools").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let tools = tools
        .as_array()
        .ok_or(AnthropicAdapterError::Incompatible("tools"))?;
    let mut translated = Vec::with_capacity(tools.len());
    for tool in tools {
        let tool = tool
            .as_object()
            .ok_or(AnthropicAdapterError::Incompatible("tool"))?;
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            return Err(AnthropicAdapterError::Incompatible("tool type"));
        }
        let function = tool
            .get("function")
            .and_then(Value::as_object)
            .ok_or(AnthropicAdapterError::Incompatible("tool function"))?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or(AnthropicAdapterError::Incompatible("tool name"))?;
        let schema = function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        if !schema.is_object() {
            return Err(AnthropicAdapterError::Incompatible("tool schema"));
        }
        let mut translated_tool = Map::from_iter([
            ("name".to_string(), Value::String(name.to_string())),
            ("input_schema".to_string(), schema),
        ]);
        if let Some(description) = function.get("description").and_then(Value::as_str) {
            translated_tool.insert(
                "description".to_string(),
                Value::String(description.to_string()),
            );
        }
        translated.push(Value::Object(translated_tool));
    }

    match source.get("tool_choice").filter(|value| !value.is_null()) {
        Some(Value::String(choice)) if choice == "none" => return Ok(()),
        Some(Value::String(choice)) if choice == "auto" => {
            destination.insert("tool_choice".to_string(), json!({"type": "auto"}));
        }
        Some(Value::String(choice)) if choice == "required" => {
            destination.insert("tool_choice".to_string(), json!({"type": "any"}));
        }
        Some(Value::Object(choice))
            if choice.get("type").and_then(Value::as_str) == Some("function") =>
        {
            let name = choice
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .ok_or(AnthropicAdapterError::Incompatible("tool_choice"))?;
            destination.insert(
                "tool_choice".to_string(),
                json!({"type": "tool", "name": name}),
            );
        }
        Some(_) => return Err(AnthropicAdapterError::Incompatible("tool_choice")),
        None => {}
    }
    destination.insert("tools".to_string(), Value::Array(translated));
    Ok(())
}

fn requested_max_tokens(source: &Map<String, Value>) -> Option<u32> {
    ["max_completion_tokens", "max_tokens"]
        .iter()
        .find_map(|field| source.get(*field).and_then(Value::as_u64))
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn requested_reasoning_effort(source: &Map<String, Value>) -> Option<ReasoningEffort> {
    source
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .or_else(|| {
            source
                .get("reasoning")
                .and_then(Value::as_object)
                .and_then(|reasoning| reasoning.get("effort"))
                .and_then(Value::as_str)
        })
        .and_then(parse_effort)
}

fn parse_effort(value: &str) -> Option<ReasoningEffort> {
    match value {
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "xhigh" => Some(ReasoningEffort::Xhigh),
        _ => None,
    }
}

fn thinking_budget(effort: ReasoningEffort, max_tokens: u32) -> Option<u32> {
    let desired = match effort {
        ReasoningEffort::Low => 1_024,
        ReasoningEffort::Medium => 4_096,
        ReasoningEffort::High => 16_384,
        ReasoningEffort::Xhigh => 32_768,
    };
    (max_tokens > 1_024).then(|| desired.min(max_tokens - 1))
}

fn copy_number(
    source: &Map<String, Value>,
    destination: &mut Map<String, Value>,
    field: &str,
) -> Result<(), AnthropicAdapterError> {
    if let Some(value) = source.get(field).filter(|value| !value.is_null()) {
        if !value.is_number() {
            return Err(AnthropicAdapterError::Incompatible("sampling value"));
        }
        destination.insert(field.to_string(), value.clone());
    }
    Ok(())
}

fn copy_stop(
    source: &Map<String, Value>,
    destination: &mut Map<String, Value>,
) -> Result<(), AnthropicAdapterError> {
    let Some(stop) = source.get("stop").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let stop_sequences = match stop {
        Value::String(_) => vec![stop.clone()],
        Value::Array(values) if values.iter().all(Value::is_string) => values.clone(),
        _ => return Err(AnthropicAdapterError::Incompatible("stop")),
    };
    destination.insert("stop_sequences".to_string(), Value::Array(stop_sequences));
    Ok(())
}

pub(super) fn translate_message_response(
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
            _ => return Err(AnthropicAdapterError::Response),
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
    let response = json!({
        "id": id,
        "object": "chat.completion",
        "created": unix_timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": Value::Object(assistant),
            "finish_reason": finish_reason(message.get("stop_reason").and_then(Value::as_str))
        }],
        "usage": translate_usage(message.get("usage"))
    });
    serde_json::to_vec(&response)
        .map(Bytes::from)
        .map_err(|_| AnthropicAdapterError::Serialization)
}

pub(super) fn open_ai_error_body(code: &str) -> Bytes {
    serde_json::to_vec(&json!({
        "error": {
            "message": "provider returned an error response",
            "type": "upstream_error",
            "code": code
        }
    }))
    .map(Bytes::from)
    .expect("the fixed provider error body serializes")
}

/// Incremental Anthropic SSE to OpenAI chat-completion SSE translator.
pub(super) struct AnthropicSseTranslator {
    buffer: Vec<u8>,
    id: String,
    model: String,
    created: u64,
    tool_indices: BTreeMap<u64, u64>,
    next_tool_index: u64,
    done: bool,
}

impl AnthropicSseTranslator {
    pub(super) fn new(model: &str) -> Self {
        Self {
            buffer: Vec::new(),
            id: format!("chatcmpl-{}", Uuid::new_v4()),
            model: model.to_string(),
            created: unix_timestamp(),
            tool_indices: BTreeMap::new(),
            next_tool_index: 0,
            done: false,
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Bytes>, AnthropicAdapterError> {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_SSE_EVENT_BYTES {
            return Err(AnthropicAdapterError::Response);
        }
        self.buffer.extend_from_slice(bytes);
        let mut output = Vec::new();
        while let Some((end, delimiter)) = event_boundary(&self.buffer) {
            let event = self.buffer.drain(..end).collect::<Vec<_>>();
            self.buffer.drain(..delimiter);
            if let Some(translated) = self.translate_event(&event)? {
                output.push(translated);
            }
        }
        Ok(output)
    }

    pub(super) fn finish(&mut self) -> Result<Vec<Bytes>, AnthropicAdapterError> {
        if !self.buffer.iter().all(u8::is_ascii_whitespace) || !self.done {
            return Err(AnthropicAdapterError::Response);
        }
        Ok(Vec::new())
    }

    fn translate_event(&mut self, event: &[u8]) -> Result<Option<Bytes>, AnthropicAdapterError> {
        let data = sse_data(event)?;
        if data.is_empty() {
            return Ok(None);
        }
        let value: Value =
            serde_json::from_slice(&data).map_err(|_| AnthropicAdapterError::Response)?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
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
                let usage = translate_usage(value.get("usage"));
                Ok(Some(self.chunk(
                    json!({}),
                    finish_reason(stop_reason),
                    Some(usage),
                )?))
            }
            "message_stop" => {
                self.done = true;
                Ok(Some(Bytes::from_static(b"data: [DONE]\n\n")))
            }
            "error" => Err(AnthropicAdapterError::Response),
            _ => Err(AnthropicAdapterError::Response),
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
            _ => Err(AnthropicAdapterError::Response),
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
            _ => Err(AnthropicAdapterError::Response),
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

fn event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2))
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| (position, 4))
        })
}

fn sse_data(event: &[u8]) -> Result<Vec<u8>, AnthropicAdapterError> {
    let text = std::str::from_utf8(event).map_err(|_| AnthropicAdapterError::Response)?;
    let mut data = Vec::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push(b'\n');
            }
            data.extend_from_slice(value.strip_prefix(' ').unwrap_or(value).as_bytes());
        }
    }
    Ok(data)
}

fn finish_reason(reason: Option<&str>) -> Value {
    match reason {
        Some("end_turn" | "stop_sequence") => Value::String("stop".to_string()),
        Some("max_tokens") => Value::String("length".to_string()),
        Some("tool_use") => Value::String("tool_calls".to_string()),
        Some(_) => Value::String("stop".to_string()),
        None => Value::Null,
    }
}

fn translate_usage(usage: Option<&Value>) -> Value {
    let input = usage
        .and_then(Value::as_object)
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let output = usage
        .and_then(Value::as_object)
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": input.saturating_add(output)
    })
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[derive(Debug, Error)]
pub(super) enum AnthropicAdapterError {
    #[error("request is incompatible with the Anthropic adapter ({0})")]
    Incompatible(&'static str),
    #[error("could not serialize an Anthropic-compatible payload")]
    Serialization,
    #[error("Anthropic-compatible provider returned an invalid response")]
    Response,
    #[error(transparent)]
    Request(#[from] crate::gateway::request::GatewayRequestError),
}

impl AnthropicAdapterError {
    pub(super) const fn is_incompatible(&self) -> bool {
        matches!(self, Self::Incompatible(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{fabric::FabricConfig, request::GatewayRequest};

    fn request(value: Value) -> GatewayRequest {
        GatewayRequest::parse(&serde_json::to_vec(&value).expect("request JSON"))
            .expect("gateway request")
    }

    #[test]
    fn open_ai_messages_tools_and_reasoning_translate_explicitly() {
        let config = FabricConfig::from_toml(include_str!("../../../config.toml"))
            .expect("repository config");
        let provider = &config.providers["kimi"];
        let request = request(json!({
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
            "tools": [{
                "type": "function",
                "function": {
                    "name": "read_file",
                    "parameters": {"type": "object", "properties": {}}
                }
            }]
        }));

        let translated =
            build_request(provider, &request, ReasoningEffort::High).expect("Anthropic request");
        let body: Value = serde_json::from_slice(&translated.body).expect("translated JSON");
        assert_eq!(body["model"], "k3");
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["system"][0]["text"], "Be concise.");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["thinking"]["budget_tokens"], 8191);
    }

    #[test]
    fn anthropic_message_response_becomes_open_ai_chat_completion() {
        let response = json!({
            "id": "msg-1",
            "type": "message",
            "model": "k3",
            "content": [
                {"type": "thinking", "thinking": "reason"},
                {"type": "text", "text": "answer"},
                {"type": "tool_use", "id": "call-1", "name": "read_file", "input": {"path": "README.md"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 4}
        });
        let translated = translate_message_response(
            &serde_json::to_vec(&response).expect("response JSON"),
            "fallback",
        )
        .expect("translated response");
        let translated: Value = serde_json::from_slice(&translated).expect("OpenAI JSON");
        assert_eq!(translated["object"], "chat.completion");
        assert_eq!(translated["choices"][0]["message"]["content"], "answer");
        assert_eq!(
            translated["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"README.md\"}"
        );
        assert_eq!(translated["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(translated["usage"]["total_tokens"], 14);
    }

    #[test]
    fn fragmented_anthropic_sse_is_incrementally_translated() {
        let input = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"model\":\"k3\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"Hi\"}}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        let mut translator = AnthropicSseTranslator::new("fallback");
        let mut output = Vec::new();
        for chunk in input.as_bytes().chunks(3) {
            output.extend(translator.push(chunk).expect("fragment"));
        }
        output.extend(translator.finish().expect("complete stream"));
        let output = output
            .into_iter()
            .flat_map(|bytes| bytes.to_vec())
            .collect::<Vec<_>>();
        let output = std::str::from_utf8(&output).expect("UTF-8");
        assert!(output.contains("chat.completion.chunk"), "{output}");
        assert!(
            output.contains("\\\"content\\\":\\\"Hi\\\"") || output.contains("\"content\":\"Hi\""),
            "{output}"
        );
        assert!(output.contains("data: [DONE]"), "{output}");
    }

    #[test]
    fn structured_output_rejects_before_provider_disclosure() {
        let config = FabricConfig::from_toml(include_str!("../../../config.toml"))
            .expect("repository config");
        let error = build_request(
            &config.providers["kimi"],
            &request(json!({
                "model": "cloud-sota",
                "messages": [{"role": "user", "content": "answer"}],
                "response_format": {"type": "json_schema", "json_schema": {"name": "answer"}}
            })),
            ReasoningEffort::High,
        )
        .expect_err("unsupported structured output");
        assert!(error.is_incompatible());
    }
}
