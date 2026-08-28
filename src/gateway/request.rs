//! Minimally parsed, schema-preserving chat-completion requests.

use crate::gateway::fabric::LocalCapability;
use bytes::Bytes;
use serde::{Serialize, Serializer, ser::SerializeMap as _};
use serde_json::{Map, Value};
use std::{collections::BTreeSet, sync::OnceLock};
use thiserror::Error;

const MAX_VIRTUAL_MODEL_BYTES: usize = 128;

/// A feature inferred from the request envelope without rewriting messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RequestFeature {
    /// Feature which can be enabled explicitly for a local upstream.
    Capability(LocalCapability),
    /// OpenRouter server-side plugins have no local equivalent.
    OpenRouterPlugins,
    /// A non-text output modality has no initial local equivalent.
    NonTextOutput,
    /// An unrecognized or malformed message or content block must fail closed.
    UnsupportedContent,
}

/// Bounded chat request retaining both original bytes and parsed JSON.
#[derive(Debug)]
pub struct GatewayRequest {
    body: Map<String, Value>,
    model: String,
    features: OnceLock<BTreeSet<RequestFeature>>,
}

impl GatewayRequest {
    /// Parse the minimum envelope Octoroute needs for routing.
    pub fn parse(bytes: &[u8]) -> Result<Self, GatewayRequestError> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|error| GatewayRequestError::Json {
                line: error.line(),
                column: error.column(),
            })?;
        let Value::Object(body) = value else {
            return Err(invalid("body", "must be a JSON object"));
        };

        let model = body
            .get("model")
            .and_then(Value::as_str)
            .filter(|model| valid_virtual_model(model))
            .ok_or_else(|| {
                invalid(
                    "model",
                    "must use 1..=128 ASCII letters, digits, dots, underscores, or hyphens",
                )
            })?
            .to_string();

        body.get("messages")
            .and_then(Value::as_array)
            .filter(|messages| !messages.is_empty())
            .ok_or_else(|| invalid("messages", "must be a non-empty array"))?;

        match body.get("stream") {
            None | Some(Value::Null | Value::Bool(_)) => {}
            Some(_) => return Err(invalid("stream", "must be a boolean")),
        }

        Ok(Self {
            body,
            model,
            features: OnceLock::new(),
        })
    }

    /// Requested model identifier.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Features which constrain local eligibility.
    pub fn features(&self) -> &BTreeSet<RequestFeature> {
        self.features.get_or_init(|| infer_features(&self.body))
    }

    /// Resolve the output-token reservation used for local context admission.
    pub fn output_token_budget(
        &self,
        default_max_output_tokens: u32,
    ) -> Result<u32, GatewayRequestError> {
        for field in ["max_completion_tokens", "max_tokens"] {
            if let Some(value) = self.body.get(field).filter(|value| !value.is_null()) {
                let tokens = value.as_u64().filter(|tokens| *tokens > 0).ok_or_else(|| {
                    invalid(field, "must be a positive integer within the u32 range")
                })?;
                return u32::try_from(tokens).map_err(|_| {
                    invalid(field, "must be a positive integer within the u32 range")
                });
            }
        }
        if default_max_output_tokens == 0 {
            return Err(invalid(
                "default_max_output_tokens",
                "must be a positive integer",
            ));
        }
        Ok(default_max_output_tokens)
    }

    /// Consume the complete body and patch only its model field.
    pub fn into_body_for_model(mut self, model: &str) -> Result<Value, GatewayRequestError> {
        validate_destination_model(model)?;
        self.body
            .insert("model".to_string(), Value::String(model.to_string()));
        Ok(Value::Object(self.body))
    }

    /// Serialize one destination-specific body for reuse across local probes and dispatch.
    pub fn body_bytes_for_model(&self, model: &str) -> Result<Bytes, GatewayRequestError> {
        validate_destination_model(model)?;
        serde_json::to_vec(&BodyWithModel {
            body: &self.body,
            model,
        })
        .map(Bytes::from)
        .map_err(|_| GatewayRequestError::Serialization)
    }

    /// Serialize a local body, supplying the pool reasoning default only when
    /// the caller omitted both supported reasoning controls.
    pub(crate) fn body_bytes_for_model_with_reasoning_default(
        &self,
        model: &str,
        reasoning_effort: &str,
    ) -> Result<Bytes, GatewayRequestError> {
        let mut body = self.body_value_for_model(model)?;
        let object = body
            .as_object_mut()
            .expect("gateway request bodies are validated objects");
        let has_reasoning_control = ["reasoning_effort", "reasoning"]
            .iter()
            .any(|field| object.get(*field).is_some_and(|value| !value.is_null()));
        if !has_reasoning_control {
            object.insert(
                "reasoning_effort".to_string(),
                Value::String(reasoning_effort.to_string()),
            );
        }
        serde_json::to_vec(&body)
            .map(Bytes::from)
            .map_err(|_| GatewayRequestError::Serialization)
    }

    /// Clone the schema-preserving body while replacing only the destination model.
    pub(crate) fn body_value_for_model(&self, model: &str) -> Result<Value, GatewayRequestError> {
        validate_destination_model(model)?;
        let mut body = self.body.clone();
        body.insert("model".to_string(), Value::String(model.to_string()));
        Ok(Value::Object(body))
    }
}

/// Safe request validation failures which never include body contents.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GatewayRequestError {
    /// Body was not valid JSON.
    #[error("invalid JSON request body at line {line}, column {column}")]
    Json {
        /// One-based parser line.
        line: usize,
        /// One-based parser column.
        column: usize,
    },
    /// Minimum OpenAI envelope validation failed.
    #[error("invalid `{field}`: {message}")]
    Invalid {
        /// Invalid field.
        field: String,
        /// Safe explanation.
        message: String,
    },
    /// A validated JSON value could not be serialized for an upstream.
    #[error("could not serialize the validated request body")]
    Serialization,
}

fn infer_features(body: &Map<String, Value>) -> BTreeSet<RequestFeature> {
    let mut features = BTreeSet::from([RequestFeature::Capability(LocalCapability::Chat)]);

    if body.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        features.insert(RequestFeature::Capability(LocalCapability::Stream));
    }
    if nonempty_array(body.get("tools")) || present(body.get("tool_choice")) {
        features.insert(RequestFeature::Capability(LocalCapability::Tools));
    }
    if body
        .get("response_format")
        .and_then(Value::as_object)
        .and_then(|format| format.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind != "text")
    {
        features.insert(RequestFeature::Capability(
            LocalCapability::StructuredOutput,
        ));
    }
    if ["reasoning", "reasoning_effort", "include_reasoning"]
        .iter()
        .any(|field| present(body.get(*field)))
    {
        features.insert(RequestFeature::Capability(LocalCapability::Reasoning));
    }
    if nonempty_array(body.get("plugins")) {
        features.insert(RequestFeature::OpenRouterPlugins);
    }
    if body
        .get("modalities")
        .and_then(Value::as_array)
        .is_some_and(|modalities| {
            modalities
                .iter()
                .any(|modality| modality.as_str().is_some_and(|value| value != "text"))
        })
    {
        features.insert(RequestFeature::NonTextOutput);
    }

    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        infer_message_features(messages, &mut features);
    }

    features
}

fn infer_message_features(messages: &[Value], features: &mut BTreeSet<RequestFeature>) {
    for message in messages {
        let Some(message) = message.as_object() else {
            features.insert(RequestFeature::UnsupportedContent);
            continue;
        };
        let Some(role) = message
            .get("role")
            .and_then(Value::as_str)
            .filter(|role| !role.is_empty())
        else {
            features.insert(RequestFeature::UnsupportedContent);
            continue;
        };
        if !matches!(role, "developer" | "system" | "user" | "assistant" | "tool") {
            features.insert(RequestFeature::UnsupportedContent);
        }

        let has_tool_calls = match message.get("tool_calls") {
            None | Some(Value::Null) => false,
            Some(Value::Array(calls)) if !calls.is_empty() => {
                features.insert(RequestFeature::Capability(LocalCapability::Tools));
                if !calls.iter().all(|call| valid_tool_call_id(call).is_some()) {
                    features.insert(RequestFeature::UnsupportedContent);
                }
                true
            }
            Some(_) => {
                features.insert(RequestFeature::UnsupportedContent);
                true
            }
        };
        if present(message.get("function_call")) {
            features.insert(RequestFeature::Capability(LocalCapability::Tools));
            features.insert(RequestFeature::UnsupportedContent);
        }
        if role == "tool" {
            features.insert(RequestFeature::Capability(LocalCapability::Tools));
            if !message.get("tool_call_id").is_some_and(Value::is_string) {
                features.insert(RequestFeature::UnsupportedContent);
            }
        }

        match message.get("content") {
            Some(Value::String(_)) => {}
            Some(Value::Array(blocks)) if !blocks.is_empty() => {
                for block in blocks {
                    infer_content_block_feature(block, role, features);
                }
            }
            None | Some(Value::Null) if role == "assistant" && has_tool_calls => {}
            _ => {
                features.insert(RequestFeature::UnsupportedContent);
            }
        }
    }
}

pub(super) fn valid_tool_call_id(call: &Value) -> Option<&str> {
    let call = call.as_object()?;
    let id = call.get("id")?.as_str().filter(|id| !id.is_empty())?;
    if call.get("type")?.as_str()? != "function" {
        return None;
    }
    let function = call.get("function")?.as_object()?;
    function
        .get("name")?
        .as_str()
        .filter(|name| !name.is_empty())?;
    function.get("arguments")?.as_str()?;
    Some(id)
}

fn infer_content_block_feature(block: &Value, role: &str, features: &mut BTreeSet<RequestFeature>) {
    let Some(block) = block.as_object() else {
        features.insert(RequestFeature::UnsupportedContent);
        return;
    };
    let valid_text = || block.get("text").is_some_and(Value::is_string);
    let valid_media = |field: &str, value_fields: &[&str]| {
        block
            .get(field)
            .and_then(Value::as_object)
            .is_some_and(|media| {
                value_fields.iter().any(|field| {
                    media
                        .get(*field)
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.is_empty())
                })
            })
    };
    let capability = match block.get("type").and_then(Value::as_str) {
        Some("text") if valid_text() => None,
        Some("image_url") if role == "user" && valid_media("image_url", &["url"]) => {
            Some(LocalCapability::ImageInput)
        }
        Some("input_audio") if role == "user" && valid_media("input_audio", &["data", "url"]) => {
            Some(LocalCapability::AudioInput)
        }
        Some("input_video") if role == "user" && valid_media("input_video", &["data", "url"]) => {
            Some(LocalCapability::VideoInput)
        }
        _ => {
            features.insert(RequestFeature::UnsupportedContent);
            return;
        }
    };
    if let Some(capability) = capability {
        features.insert(RequestFeature::Capability(capability));
    }
}

fn present(value: Option<&Value>) -> bool {
    value.is_some_and(|value| !value.is_null())
}

fn nonempty_array(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .is_some_and(|values| !values.is_empty())
}

struct BodyWithModel<'a> {
    body: &'a Map<String, Value>,
    model: &'a str,
}

impl Serialize for BodyWithModel<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.body.len()))?;
        for (key, value) in self.body {
            if key == "model" {
                map.serialize_entry(key, self.model)?;
            } else {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}

fn validate_destination_model(model: &str) -> Result<(), GatewayRequestError> {
    if model.trim().is_empty() {
        return Err(invalid("model", "destination model must not be empty"));
    }
    Ok(())
}

fn valid_virtual_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= MAX_VIRTUAL_MODEL_BYTES
        && model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn invalid(field: impl Into<String>, message: impl Into<String>) -> GatewayRequestError {
    GatewayRequestError::Invalid {
        field: field.into(),
        message: message.into(),
    }
}
