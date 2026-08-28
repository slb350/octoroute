//! OpenAI chat-completion request to Anthropic Messages request translation.

use super::AnthropicAdapterError;
use crate::gateway::fabric::{ProviderConfig, ReasoningEffort};
use crate::gateway::request::GatewayRequest;
use bytes::Bytes;
use serde_json::{Map, Number, Value, json};

#[derive(Debug)]
pub(crate) struct AnthropicRequest {
    pub(crate) body: Bytes,
}

pub(crate) fn build_request(
    config: &ProviderConfig,
    request: &GatewayRequest,
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
    reject_late_system_messages(source_messages)?;
    if messages.is_empty() {
        return Err(AnthropicAdapterError::Incompatible("messages"));
    }

    let max_tokens = requested_max_tokens(source)?
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
    copy_stop(source, &mut body)?;
    translate_tools(source, &mut body)?;

    // Thinking is opt-in: the caller's reasoning control or the provider's own
    // configuration must ask for it. A route-wide default must not silently
    // enable thinking on a provider that was never configured for it.
    let effort = requested_reasoning_effort(source)?.or(config.reasoning_effort);
    let thinking = effort.and_then(|effort| thinking_budget(effort, max_tokens));
    if let Some(budget) = thinking {
        body.insert(
            "thinking".to_string(),
            json!({"type": "enabled", "budget_tokens": budget}),
        );
    } else {
        // Anthropic rejects `temperature`, `top_p`, and `top_k` alongside
        // enabled thinking, so sampling controls are forwarded only when the
        // request is not a thinking request.
        copy_number(source, &mut body, "top_p")?;
        copy_number(source, &mut body, "top_k")?;
        copy_number(source, &mut body, "temperature")?;
        if !body.contains_key("temperature")
            && let Some(temperature) = config.temperature
        {
            body.insert(
                "temperature".to_string(),
                Value::Number(
                    Number::from_f64(temperature).ok_or(AnthropicAdapterError::Serialization)?,
                ),
            );
        }
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
        .filter(|value| !value.is_null())
        .is_some_and(|value| value.as_u64() != Some(1))
        || source
            .get("modalities")
            .filter(|value| !value.is_null())
            .is_some_and(|value| {
                value
                    .as_array()
                    .is_none_or(|values| values.iter().any(|value| value.as_str() != Some("text")))
            })
        || source
            .get("response_format")
            .filter(|value| !value.is_null())
            .is_some_and(|value| {
                value
                    .as_object()
                    .and_then(|value| value.get("type"))
                    .and_then(Value::as_str)
                    != Some("text")
            })
        || UNMAPPED_OPEN_AI_FIELDS
            .iter()
            .any(|field| source.get(*field).is_some_and(|value| !value.is_null()))
    {
        return Err(AnthropicAdapterError::Incompatible("request feature"));
    }
    Ok(())
}

/// OpenAI request fields with no verified Anthropic Messages equivalent.
///
/// Every one of these changes what the caller gets back, so a request carrying
/// one is incompatible with this provider and falls through to the next route
/// step rather than being answered by a silently different request.
const UNMAPPED_OPEN_AI_FIELDS: [&str; 12] = [
    "logprobs",
    "top_logprobs",
    "audio",
    "seed",
    "frequency_penalty",
    "presence_penalty",
    "logit_bias",
    "stream_options",
    "parallel_tool_calls",
    "user",
    "metadata",
    "service_tier",
];

/// Reject a `system` or `developer` turn that appears after conversation content.
///
/// Anthropic carries system text in a dedicated top-level field, so a mid-
/// conversation instruction cannot be represented in place. Hoisting it silently
/// promotes it to a global instruction and erases the turn boundary around it, so
/// the request fails closed to the next route step instead.
fn reject_late_system_messages(source_messages: &[Value]) -> Result<(), AnthropicAdapterError> {
    let mut saw_conversation = false;
    for message in source_messages {
        let role = message
            .as_object()
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .ok_or(AnthropicAdapterError::Incompatible("message role"))?;
        match role {
            "system" | "developer" if saw_conversation => {
                return Err(AnthropicAdapterError::Incompatible(
                    "system message after conversation content",
                ));
            }
            "system" | "developer" => {}
            _ => saw_conversation = true,
        }
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
        // The tools array stays even when the caller forbids new calls: prior
        // `tool_use`/`tool_result` blocks in the history are only valid
        // alongside the definitions they name.
        Some(Value::String(choice)) if choice == "none" => {
            destination.insert("tool_choice".to_string(), json!({"type": "none"}));
        }
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

fn requested_max_tokens(source: &Map<String, Value>) -> Result<Option<u32>, AnthropicAdapterError> {
    for field in ["max_completion_tokens", "max_tokens"] {
        let Some(value) = source.get(field).filter(|value| !value.is_null()) else {
            continue;
        };
        let value = value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or(AnthropicAdapterError::Incompatible("max_tokens"))?;
        return Ok(Some(value));
    }
    Ok(None)
}

fn requested_reasoning_effort(
    source: &Map<String, Value>,
) -> Result<Option<ReasoningEffort>, AnthropicAdapterError> {
    let direct = source
        .get("reasoning_effort")
        .filter(|value| !value.is_null());
    let nested = match source.get("reasoning").filter(|value| !value.is_null()) {
        Some(Value::Object(reasoning)) => reasoning.get("effort").filter(|value| !value.is_null()),
        Some(_) => return Err(AnthropicAdapterError::Incompatible("reasoning effort")),
        None => None,
    };
    match direct.or(nested) {
        Some(Value::String(value)) => parse_effort(value)
            .map(Some)
            .ok_or(AnthropicAdapterError::Incompatible("reasoning effort")),
        Some(_) => Err(AnthropicAdapterError::Incompatible("reasoning effort")),
        None => Ok(None),
    }
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

/// Anthropic's minimum accepted thinking budget.
const MIN_THINKING_BUDGET_TOKENS: u32 = 1_024;

/// Resolve the thinking budget, or `None` when thinking cannot be afforded.
///
/// `max_tokens` is the total for thinking plus the visible answer, so the budget
/// claims at most half of it and the remainder stays available for the answer.
/// Below Anthropic's 1024-token minimum there is no affordable budget and the
/// request is sent without thinking rather than with an unusable answer
/// allowance.
fn thinking_budget(effort: ReasoningEffort, max_tokens: u32) -> Option<u32> {
    let desired = match effort {
        ReasoningEffort::Low => 1_024,
        ReasoningEffort::Medium => 4_096,
        ReasoningEffort::High => 16_384,
        ReasoningEffort::Xhigh => 32_768,
    };
    let budget = desired.min(max_tokens / 2);
    (budget >= MIN_THINKING_BUDGET_TOKENS).then_some(budget)
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
