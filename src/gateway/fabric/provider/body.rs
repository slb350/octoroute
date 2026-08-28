//! OpenAI-protocol request body construction and the OpenRouter Auto profile.

use super::ProviderRequestError;
use crate::gateway::fabric::{ProviderConfig, ProviderProfile};
use crate::gateway::request::GatewayRequest;
use bytes::Bytes;
use serde_json::{Map, Number, Value};

const OPENROUTER_AUTO_PLUGIN: &str = "auto-router";
const OPENROUTER_COST_QUALITY_TRADEOFF: u64 = 9;

pub(super) fn build_open_ai_body(
    config: &ProviderConfig,
    request: &GatewayRequest,
) -> Result<Bytes, ProviderRequestError> {
    let mut body = request.body_value_for_model(&config.model)?;
    let object = body
        .as_object_mut()
        .expect("gateway request bodies are validated objects");

    // Only a provider explicitly configured for reasoning receives the field.
    // A route-wide default would send `reasoning_effort` to every OpenAI-protocol
    // model whether or not it accepts one, which mutates more than the model and
    // server-owned Auto policy.
    if let Some(effort) = config.reasoning_effort
        && !present(object, "reasoning_effort")
        && !present(object, "reasoning")
    {
        object.insert(
            "reasoning_effort".to_string(),
            Value::String(effort.as_str().to_string()),
        );
    }
    if let Some(temperature) = config.temperature
        && !present(object, "temperature")
    {
        let number = Number::from_f64(temperature).ok_or(ProviderRequestError::Serialization)?;
        object.insert("temperature".to_string(), Value::Number(number));
    }
    if let Some(max_tokens) = config.max_tokens
        && !present(object, "max_tokens")
        && !present(object, "max_completion_tokens")
    {
        object.insert(
            "max_tokens".to_string(),
            Value::Number(Number::from(max_tokens)),
        );
    }
    if config.profile == ProviderProfile::OpenRouterAuto {
        apply_openrouter_auto_profile(object)?;
    }

    serde_json::to_vec(&body)
        .map(Bytes::from)
        .map_err(|_| ProviderRequestError::Serialization)
}

fn apply_openrouter_auto_profile(
    body: &mut Map<String, Value>,
) -> Result<(), ProviderRequestError> {
    let plugins = body
        .entry("plugins")
        .or_insert_with(|| Value::Array(Vec::new()));
    if plugins.is_null() {
        *plugins = Value::Array(Vec::new());
    }
    let Value::Array(plugins) = plugins else {
        return Err(ProviderRequestError::InvalidOpenRouterPlugins);
    };

    let matching_index = {
        let mut matching = plugins.iter().enumerate().filter_map(|(index, plugin)| {
            plugin
                .as_object()
                .and_then(|plugin| plugin.get("id"))
                .and_then(Value::as_str)
                .filter(|id| *id == OPENROUTER_AUTO_PLUGIN)
                .map(|_| index)
        });
        let first = matching.next();
        if matching.next().is_some() {
            return Err(ProviderRequestError::InvalidOpenRouterPlugins);
        }
        first
    };

    let plugin = if let Some(index) = matching_index {
        plugins[index]
            .as_object_mut()
            .ok_or(ProviderRequestError::InvalidOpenRouterPlugins)?
    } else {
        plugins.push(Value::Object(Map::new()));
        plugins
            .last_mut()
            .and_then(Value::as_object_mut)
            .expect("the appended OpenRouter profile is an object")
    };
    plugin.insert(
        "id".to_string(),
        Value::String(OPENROUTER_AUTO_PLUGIN.to_string()),
    );
    plugin.insert(
        "cost_quality_tradeoff".to_string(),
        Value::Number(Number::from(OPENROUTER_COST_QUALITY_TRADEOFF)),
    );
    plugin.remove("allowed_models");
    Ok(())
}

fn present(body: &Map<String, Value>, field: &str) -> bool {
    body.get(field).is_some_and(|value| !value.is_null())
}
