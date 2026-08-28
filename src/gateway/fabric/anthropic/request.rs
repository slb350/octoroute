//! OpenAI chat-completion request to Anthropic Messages request translation.
//!
//! The concerns are split by what they guard: [`fields`] rejects anything
//! without a verified mapping, [`messages`] and [`tools`] translate the
//! conversation and the tool contract, and [`params`] resolves the token
//! limits, reasoning controls, and sampling parameters.

mod fields;
mod messages;
mod params;
mod tools;

use super::AnthropicAdapterError;
use crate::gateway::fabric::ProviderConfig;
use crate::gateway::request::GatewayRequest;
use bytes::Bytes;
use fields::{reject_late_system_messages, reject_unsupported_request_fields};
use messages::translate_message;
use params::{
    RequestedThinking, affordable_budget, copy_stop, copy_top_k, copy_unit_interval,
    requested_max_tokens, requested_thinking, thinking_budget,
};
use serde_json::{Map, Number, Value, json};
use tools::translate_tools;

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
    // Anthropic requires the conversation to open on a `user` turn. Sending an
    // assistant-first conversation buys a 400 the client only sees after the
    // route has committed to this provider, so it fails the step closed here.
    if messages
        .first()
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str)
        != Some("user")
    {
        return Err(AnthropicAdapterError::Incompatible("first message role"));
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
    let thinking = match requested_thinking(source)? {
        Some(RequestedThinking::Disabled) => None,
        Some(RequestedThinking::Effort(effort)) => thinking_budget(effort, max_tokens),
        Some(RequestedThinking::Budget(budget)) => affordable_budget(budget, max_tokens),
        None => config
            .reasoning_effort
            .and_then(|effort| thinking_budget(effort, max_tokens)),
    };
    if let Some(budget) = thinking {
        body.insert(
            "thinking".to_string(),
            json!({"type": "enabled", "budget_tokens": budget}),
        );
    } else {
        // Anthropic rejects `temperature`, `top_p`, and `top_k` alongside
        // enabled thinking, so sampling controls are forwarded only when the
        // request is not a thinking request.
        copy_unit_interval(source, &mut body, "top_p")?;
        copy_top_k(source, &mut body)?;
        copy_unit_interval(source, &mut body, "temperature")?;
        if !body.contains_key("temperature")
            && let Some(temperature) = config.temperature
        {
            if !(0.0..=1.0).contains(&temperature) {
                return Err(AnthropicAdapterError::Incompatible("sampling value"));
            }
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
