//! Anthropic adapter translation tests.
//!
//! The fixtures every topic shares live here; the tests themselves are split by
//! the translation concern they exercise.

mod fields;
mod messages;
mod response;
mod streaming;
mod thinking;
mod tools;
mod usage_and_errors;

use super::AnthropicAdapterError;
use super::request::build_request;
use super::response::{AnthropicSseTranslator, open_ai_error_body, translate_message_response};
use crate::gateway::{
    fabric::{FabricConfig, ProviderConfig, ReasoningEffort},
    request::GatewayRequest,
};
use serde_json::{Value, json};

const REPOSITORY_CONFIG: &str = include_str!("../../../../config.toml");

fn config() -> FabricConfig {
    FabricConfig::from_toml(REPOSITORY_CONFIG).expect("repository config")
}

fn request(value: Value) -> GatewayRequest {
    GatewayRequest::parse(&serde_json::to_vec(&value).expect("request JSON"))
        .expect("gateway request")
}

/// Build against the shipped `kimi` provider, returning the translated body.
fn translate(value: Value) -> Value {
    let config = config();
    let translated =
        build_request(&config.providers["kimi"], &request(value)).expect("Anthropic request");
    serde_json::from_slice(&translated.body).expect("translated JSON")
}

fn chat(extra: Value) -> Value {
    let mut body = json!({
        "model": "cloud-sota",
        "messages": [{"role": "user", "content": "answer"}]
    });
    body.as_object_mut()
        .expect("object")
        .extend(extra.as_object().expect("object").clone());
    body
}

/// A complete, well-formed Anthropic stream, used wherever a test needs one.
const STREAM: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"model\":\"k3\",\"usage\":{\"input_tokens\":7}}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

fn rendered(output: &[bytes::Bytes]) -> String {
    output
        .iter()
        .map(|chunk| String::from_utf8_lossy(chunk).to_string())
        .collect()
}

/// Parse the JSON payload of one translated `data: ...` chunk.
fn chunk_json(chunk: &[u8]) -> Value {
    let text = String::from_utf8_lossy(chunk).to_string();
    let data = text.strip_prefix("data: ").expect("data prefix");
    serde_json::from_str(data.trim_end()).expect("chunk JSON")
}

/// The shipped `kimi` provider with a provider-level reasoning default applied.
fn provider_with(reasoning_effort: Option<ReasoningEffort>) -> ProviderConfig {
    let mut provider = config().providers["kimi"].clone();
    provider.reasoning_effort = reasoning_effort;
    provider
}

#[test]
fn only_incompatible_errors_are_classified_as_incompatible() {
    for (error, expected) in [
        (AnthropicAdapterError::Incompatible("test"), true),
        (AnthropicAdapterError::Serialization, false),
        (AnthropicAdapterError::Response, false),
    ] {
        assert_eq!(error.is_incompatible(), expected, "{error:?}");
    }
}
