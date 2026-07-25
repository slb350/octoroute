//! Schema-preserving OpenRouter request mutation.

use crate::gateway::{
    config::OpenRouterConfig,
    request::{GatewayRequest, GatewayRequestError},
    routing::ModelIntent,
};
use serde_json::{Map, Number, Value};
use thiserror::Error;

const AUTO_ROUTER_PLUGIN_ID: &str = "auto-router";

/// Complete body prepared for the OpenRouter chat-completions endpoint.
#[derive(Debug, PartialEq)]
pub struct OpenRouterRequest {
    body: Value,
}

impl OpenRouterRequest {
    /// Patch only the selected cloud model and authoritative Auto Router fields.
    pub fn build(
        request: GatewayRequest,
        intent: &ModelIntent,
        config: &OpenRouterConfig,
    ) -> Result<Self, OpenRouterRequestError> {
        let (model, apply_auto_policy) = match intent {
            ModelIntent::Auto | ModelIntent::CloudAuto => (config.auto_model(), true),
            ModelIntent::CloudModel(model) => (model.as_str(), false),
            ModelIntent::Local => return Err(OpenRouterRequestError::LocalIntent),
        };
        let mut body = request.into_body_for_model(model)?;
        if apply_auto_policy {
            apply_auto_router_policy(&mut body, config)?;
        }
        Ok(Self { body })
    }

    /// Consume the prepared JSON body.
    pub fn into_body(self) -> Value {
        self.body
    }

    /// Borrow the prepared JSON body.
    pub fn body(&self) -> &Value {
        &self.body
    }
}

/// Safe OpenRouter request construction failures.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OpenRouterRequestError {
    /// A local-only intent reached the cloud adapter.
    #[error("local model intent cannot be sent to OpenRouter")]
    LocalIntent,
    /// Auto Router policy requires a canonical plugins array.
    #[error("invalid `plugins`: expected an array with at most one auto-router entry")]
    InvalidPlugins,
    /// Destination model validation failed.
    #[error(transparent)]
    Request(#[from] GatewayRequestError),
}

fn apply_auto_router_policy(
    body: &mut Value,
    config: &OpenRouterConfig,
) -> Result<(), OpenRouterRequestError> {
    let object = body
        .as_object_mut()
        .ok_or(OpenRouterRequestError::InvalidPlugins)?;
    let plugins = object
        .entry("plugins")
        .or_insert_with(|| Value::Array(Vec::new()));
    if plugins.is_null() {
        *plugins = Value::Array(Vec::new());
    }
    let Value::Array(plugins) = plugins else {
        return Err(OpenRouterRequestError::InvalidPlugins);
    };

    let matching_index = {
        let mut indices = plugins.iter().enumerate().filter_map(|(index, plugin)| {
            plugin
                .as_object()
                .and_then(|plugin| plugin.get("id"))
                .and_then(Value::as_str)
                .filter(|id| *id == AUTO_ROUTER_PLUGIN_ID)
                .map(|_| index)
        });
        let first = indices.next();
        if indices.next().is_some() {
            return Err(OpenRouterRequestError::InvalidPlugins);
        }
        first
    };

    let plugin = if let Some(index) = matching_index {
        plugins[index]
            .as_object_mut()
            .ok_or(OpenRouterRequestError::InvalidPlugins)?
    } else {
        plugins.push(Value::Object(Map::new()));
        let Some(Value::Object(plugin)) = plugins.last_mut() else {
            unreachable!("the appended Auto Router plugin is an object");
        };
        plugin
    };
    plugin.insert(
        "id".to_string(),
        Value::String(AUTO_ROUTER_PLUGIN_ID.to_string()),
    );
    plugin.insert(
        "cost_quality_tradeoff".to_string(),
        Value::Number(Number::from(config.cost_quality_tradeoff())),
    );
    if config.allowed_models().is_empty() {
        plugin.remove("allowed_models");
    } else {
        plugin.insert(
            "allowed_models".to_string(),
            Value::Array(
                config
                    .allowed_models()
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    Ok(())
}
