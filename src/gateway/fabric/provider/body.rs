//! OpenAI-protocol request body construction and the OpenRouter Auto profile.

use super::ProviderRequestError;
use crate::gateway::fabric::{ProviderConfig, ProviderProfile};
use crate::gateway::request::GatewayRequest;
use bytes::Bytes;
use serde_json::{Map, Number, Value};

const OPENROUTER_AUTO_PLUGIN: &str = "auto-router";
/// The cost band Octoroute's Auto policy asks for, on the current parameter.
///
/// OpenRouter deprecated `cost_quality_tradeoff` (a 0-9 scale whose top was 9,
/// this profile's value since v2) in favour of the `cost_tier` band set
/// `low | medium | high | xhigh | max`, where `max` "favors the most capable
/// models regardless of price". A request with no cost setting routes as
/// roughly the `low` band, so writing nothing would quietly trade the
/// operator's policy for the cheapest models; `max` is the top-of-scale
/// translation of the 9 this profile has always asked for.
const OPENROUTER_COST_TIER: &str = "max";
/// Client-controlled Auto Router steering fields removed from the plugin.
///
/// The Auto policy is server-owned, which is why the plugin object is rebuilt
/// from Octoroute's keys alone. `allowed_models` and `excluded_models` both
/// restrict which models the router may select, and OpenRouter documents that
/// a request `cost_tier` overrides the deprecated `cost_quality_tradeoff`, so
/// leaving any of the three in place hands the client the operator's policy.
const OPENROUTER_CLIENT_STEERING_FIELDS: [&str; 3] =
    ["allowed_models", "excluded_models", "cost_quality_tradeoff"];

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
        "cost_tier".to_string(),
        Value::String(OPENROUTER_COST_TIER.to_string()),
    );
    for field in OPENROUTER_CLIENT_STEERING_FIELDS {
        plugin.remove(field);
    }
    Ok(())
}

fn present(body: &Map<String, Value>, field: &str) -> bool {
    body.get(field).is_some_and(|value| !value.is_null())
}
