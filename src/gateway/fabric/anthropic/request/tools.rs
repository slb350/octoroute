//! Tool declaration and tool-choice translation.

use super::fields::reject_unknown_keys;
use crate::gateway::fabric::anthropic::AnthropicAdapterError;
use serde_json::{Map, Value, json};

pub(super) fn translate_tools(
    source: &Map<String, Value>,
    destination: &mut Map<String, Value>,
) -> Result<(), AnthropicAdapterError> {
    let choice = source.get("tool_choice").filter(|value| !value.is_null());
    let Some(tools) = source.get("tools").filter(|value| !value.is_null()) else {
        // A `tool_choice` naming an array that is not there is not a request
        // this adapter can honor, and dropping it changes what the model may
        // do. `"none"` is the exception: OpenAI documents it as the default
        // when no tools are present, so the caller asked for the request this
        // adapter would build anyway.
        match choice {
            None => {}
            Some(Value::String(choice)) if choice == "none" => {}
            Some(_) => return Err(AnthropicAdapterError::Incompatible("tool_choice")),
        }
        return Ok(());
    };
    let tools = tools
        .as_array()
        .ok_or(AnthropicAdapterError::Incompatible("tools"))?;
    // Anthropic rejects an empty tools array outright.
    if tools.is_empty() {
        return Err(AnthropicAdapterError::Incompatible("tools"));
    }
    let mut translated = Vec::with_capacity(tools.len());
    for tool in tools {
        let tool = tool
            .as_object()
            .ok_or(AnthropicAdapterError::Incompatible("tool"))?;
        reject_unknown_keys(tool, &["type", "function"], "tool field")?;
        if tool.get("type").and_then(Value::as_str) != Some("function") {
            return Err(AnthropicAdapterError::Incompatible("tool type"));
        }
        let function = tool
            .get("function")
            .and_then(Value::as_object)
            .ok_or(AnthropicAdapterError::Incompatible("tool function"))?;
        // `strict` is the case that matters: dropping it relaxes a schema the
        // caller asked to have enforced, and Anthropic has no equivalent knob.
        reject_unknown_keys(
            function,
            &["name", "description", "parameters"],
            "tool function field",
        )?;
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

    match choice {
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
            reject_unknown_keys(choice, &["type", "function"], "tool_choice field")?;
            let function = choice
                .get("function")
                .and_then(Value::as_object)
                .ok_or(AnthropicAdapterError::Incompatible("tool_choice"))?;
            reject_unknown_keys(function, &["name"], "tool_choice function field")?;
            let name = function
                .get("name")
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
