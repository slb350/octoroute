//! OpenAI-protocol request body construction and the OpenRouter Auto profile.

use super::ProviderRequestError;
use crate::gateway::fabric::{ProviderConfig, ProviderProfile};
use crate::gateway::request::{FieldOverride, GatewayRequest};
use bytes::Bytes;
use serde_json::{Map, Number, Value};

const OPENROUTER_AUTO_PLUGIN: &str = "auto-router";
/// The cost band Octoroute's Auto policy asks for, on the current parameter.
///
/// OpenRouter's API reference deprecates `cost_quality_tradeoff`, a 0-10
/// ceiling on which higher values favor cheaper models and whose default is
/// the 9 this profile has always written. It keeps models up to roughly the
/// 20th cost percentile. Its replacement `cost_tier` selects percentile bands,
/// and `low` is `[0, 20)`, so `low` carries the same policy onto the current
/// parameter. `max` is `[80, 100]`, the most expensive models.
const OPENROUTER_COST_TIER: &str = "low";
/// Client-controlled Auto Router steering fields removed from the plugin.
///
/// The Auto policy is server-owned. `allowed_models` and `excluded_models`
/// narrow the models the router may select, `cost_quality_tradeoff` is the
/// deprecated cost knob, `enabled: false` switches the plugin off, and
/// `pin_model` reuses the model named by client-supplied conversation history.
/// The client `cost_tier` is overwritten with the server value. Any other
/// plugin field is preserved like every other unknown request field.
const OPENROUTER_CLIENT_STEERING_FIELDS: [&str; 5] = [
    "allowed_models",
    "excluded_models",
    "cost_quality_tradeoff",
    "enabled",
    "pin_model",
];

pub(super) fn build_open_ai_body(
    config: &ProviderConfig,
    request: &GatewayRequest,
) -> Result<Bytes, ProviderRequestError> {
    let object = request.body();
    let openrouter_plugins = if config.profile == ProviderProfile::OpenRouterAuto {
        Some(openrouter_auto_plugins(object.get("plugins"))?)
    } else {
        None
    };

    let mut overrides = Vec::with_capacity(4);
    // Only a provider explicitly configured for reasoning receives the field.
    // A route-wide default would send `reasoning_effort` to every OpenAI-protocol
    // model whether or not it accepts one, which mutates more than the model and
    // server-owned Auto policy.
    if let Some(effort) = config.reasoning_effort
        && !present(object, "reasoning_effort")
        && !present(object, "reasoning")
    {
        overrides.push(("reasoning_effort", FieldOverride::Str(effort.as_str())));
    }
    if let Some(temperature) = config.temperature
        && !present(object, "temperature")
    {
        let number = Number::from_f64(temperature).ok_or(ProviderRequestError::Serialization)?;
        overrides.push(("temperature", FieldOverride::Number(number)));
    }
    if let Some(max_tokens) = config.max_tokens
        && !present(object, "max_tokens")
        && !present(object, "max_completion_tokens")
    {
        overrides.push(("max_tokens", FieldOverride::U32(max_tokens)));
    }
    if let Some(plugins) = openrouter_plugins.as_ref() {
        overrides.push(("plugins", FieldOverride::Value(plugins)));
    }

    Ok(request.body_bytes_for_model_with_overrides(&config.model, &overrides)?)
}

fn openrouter_auto_plugins(existing: Option<&Value>) -> Result<Value, ProviderRequestError> {
    let mut plugins = match existing {
        None | Some(Value::Null) => Vec::with_capacity(1),
        Some(Value::Array(plugins)) => plugins.clone(),
        Some(_) => return Err(ProviderRequestError::InvalidOpenRouterPlugins),
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
        "cost_tier".to_string(),
        Value::String(OPENROUTER_COST_TIER.to_string()),
    );
    for field in OPENROUTER_CLIENT_STEERING_FIELDS {
        plugin.remove(field);
    }
    Ok(Value::Array(plugins))
}

fn present(body: &Map<String, Value>, field: &str) -> bool {
    body.get(field).is_some_and(|value| !value.is_null())
}
