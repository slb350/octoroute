//! Message, content-block, and tool-call translation.

use super::fields::reject_unknown_keys;
use crate::gateway::fabric::anthropic::AnthropicAdapterError;
use serde_json::{Value, json};

/// The message keys OpenAI's chat-completions API emits or accepts.
///
/// The allowlist has to cover the *response* shape, not just the request shape.
/// The standard tool-calling loop is `messages.push(response.choices[0].message)`,
/// so every field OpenAI puts on a response message comes straight back in on
/// the next turn. An allowlist narrower than that makes every multi-turn
/// conversation from a raw-JSON client unroutable through an Anthropic step.
///
/// Each field here either translates or cannot change what the caller receives
/// when it is dropped:
///
/// - `name` identifies a participant. Anthropic has no per-message author
///   field, and OpenAI documents `name` as an optional hint a model may ignore,
///   so dropping it cannot answer a different question.
/// - `annotations` are URL citations attached to text the model already
///   produced. They describe a previous answer rather than instructing the next
///   one, and OpenAI emits `annotations: []` on every assistant response.
///
/// `refusal`, `audio`, and `function_call` are deliberately absent. OpenAI sets
/// each of them alongside `content: null`, and a `null` value already passes as
/// an absent key, so the round-trip works without them; a *non-null* value is
/// assistant content this adapter cannot represent, and dropping it silently
/// would erase a turn.
const PLAIN_MESSAGE_FIELDS: [&str; 3] = ["role", "content", "name"];
const ASSISTANT_MESSAGE_FIELDS: [&str; 5] =
    ["role", "content", "tool_calls", "name", "annotations"];
const TOOL_MESSAGE_FIELDS: [&str; 4] = ["role", "content", "tool_call_id", "name"];

/// `index` is the position of a call within `tool_calls`, which the array
/// itself already carries. Streaming accumulators (LangChain, the Vercel AI
/// SDK) keep it on each entry when they reassemble a tool call, so refusing it
/// rejects a reassembled request that means exactly what its array order says.
const TOOL_CALL_FIELDS: [&str; 4] = ["id", "type", "function", "index"];

pub(super) fn translate_message(
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
            reject_unknown_keys(message, &PLAIN_MESSAGE_FIELDS, "message field")?;
            system.extend(text_blocks(message.get("content"))?);
        }
        "user" => {
            reject_unknown_keys(message, &PLAIN_MESSAGE_FIELDS, "message field")?;
            push_message(messages, "user", content_blocks(message.get("content"))?);
        }
        "assistant" => {
            reject_unknown_keys(message, &ASSISTANT_MESSAGE_FIELDS, "message field")?;
            let mut blocks = match message.get("content") {
                // A tool-calling assistant turn legitimately carries no visible
                // text, in any of the three shapes OpenAI clients emit for it.
                None | Some(Value::Null) => Vec::new(),
                Some(Value::String(text)) if text.is_empty() => Vec::new(),
                Some(Value::Array(blocks)) if blocks.is_empty() => Vec::new(),
                content => content_blocks(content)?,
            };
            if let Some(tool_calls) = message.get("tool_calls") {
                translate_tool_calls(tool_calls, &mut blocks)?;
            }
            if blocks.is_empty() {
                return Err(AnthropicAdapterError::Incompatible("assistant message"));
            }
            push_message(messages, "assistant", blocks);
        }
        "tool" => {
            reject_unknown_keys(message, &TOOL_MESSAGE_FIELDS, "message field")?;
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

fn translate_tool_calls(
    tool_calls: &Value,
    blocks: &mut Vec<Value>,
) -> Result<(), AnthropicAdapterError> {
    let tool_calls = tool_calls
        .as_array()
        .ok_or(AnthropicAdapterError::Incompatible("tool_calls"))?;
    for call in tool_calls {
        let call = call
            .as_object()
            .ok_or(AnthropicAdapterError::Incompatible("tool call"))?;
        reject_unknown_keys(call, &TOOL_CALL_FIELDS, "tool call field")?;
        // Only `function` calls have a `tool_use` equivalent; a custom or
        // future call type would otherwise be translated as if it were one.
        if call.get("type").and_then(Value::as_str) != Some("function") {
            return Err(AnthropicAdapterError::Incompatible("tool call type"));
        }
        let function = call
            .get("function")
            .and_then(Value::as_object)
            .ok_or(AnthropicAdapterError::Incompatible("tool call"))?;
        reject_unknown_keys(function, &["name", "arguments"], "tool call function field")?;
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
        // Anthropic rejects an empty text block and an empty content array, so
        // forwarding either buys a committed 400 instead of a fall-forward.
        Some(Value::String(text)) if text.is_empty() => {
            Err(AnthropicAdapterError::Incompatible("empty message content"))
        }
        Some(Value::String(text)) => Ok(vec![json!({"type": "text", "text": text})]),
        Some(Value::Array(blocks)) if blocks.is_empty() => {
            Err(AnthropicAdapterError::Incompatible("empty message content"))
        }
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|block| {
                let block = block
                    .as_object()
                    .ok_or(AnthropicAdapterError::Incompatible("content block"))?;
                reject_unknown_keys(block, &["type", "text"], "content block field")?;
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    return Err(AnthropicAdapterError::Incompatible("content block"));
                }
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(AnthropicAdapterError::Incompatible("text content"))?;
                if text.is_empty() {
                    return Err(AnthropicAdapterError::Incompatible("empty message content"));
                }
                Ok(json!({"type": "text", "text": text}))
            })
            .collect(),
        _ => Err(AnthropicAdapterError::Incompatible("message content")),
    }
}

fn tool_result_content(content: Option<&Value>) -> Result<Value, AnthropicAdapterError> {
    match content {
        Some(Value::String(text)) if text.is_empty() => {
            Err(AnthropicAdapterError::Incompatible("empty tool result"))
        }
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
