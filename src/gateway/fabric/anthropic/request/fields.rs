//! Fail-closed validation of the OpenAI request fields this adapter reads.

use crate::gateway::fabric::anthropic::AnthropicAdapterError;
use serde_json::{Map, Value};

pub(super) fn reject_unsupported_request_fields(
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
    {
        return Err(AnthropicAdapterError::Incompatible("request feature"));
    }
    reject_unknown_keys(source, &MAPPED_OPEN_AI_FIELDS, "request feature")
}

/// Reject any key the adapter does not read, at whatever nesting level.
///
/// The top-level allowlist alone cannot keep the fail-closed promise: a key
/// nested inside a message, a content block, a tool, or the `reasoning` object
/// is just as capable of changing what the caller asked for, and dropping it
/// silently hands back a plausible answer to a different request. A `null`
/// value carries no instruction, so it is ignored the way an absent key is.
pub(super) fn reject_unknown_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    context: &'static str,
) -> Result<(), AnthropicAdapterError> {
    if object
        .iter()
        .any(|(key, value)| !allowed.contains(&key.as_str()) && !value.is_null())
    {
        return Err(AnthropicAdapterError::Incompatible(context));
    }
    Ok(())
}

/// Every OpenAI request field this adapter understands.
///
/// An allowlist, deliberately. The contract is that a feature without a
/// verified mapping fails as `incompatible`, and a denylist of unmapped fields
/// cannot keep that promise: it accepts and silently drops whatever OpenAI adds
/// next, so a caller gets a plausible answer to a request that was quietly
/// altered. Failing closed instead sends the request to the next route step.
///
/// Adding a field here is a claim that `build_request` reads it, or that
/// dropping it cannot change what the caller receives.
const MAPPED_OPEN_AI_FIELDS: [&str; 16] = [
    // Translated into the Anthropic request.
    "model",
    "messages",
    "max_tokens",
    "max_completion_tokens",
    "stream",
    "stop",
    "tools",
    "tool_choice",
    "reasoning_effort",
    "reasoning",
    "temperature",
    "top_p",
    "top_k",
    // Recognized, and constrained to the single value that maps cleanly.
    "n",
    "modalities",
    "response_format",
];

/// Reject a `system` or `developer` turn that appears after conversation content.
///
/// Anthropic carries system text in a dedicated top-level field, so a mid-
/// conversation instruction cannot be represented in place. Hoisting it silently
/// promotes it to a global instruction and erases the turn boundary around it, so
/// the request fails closed to the next route step instead.
pub(super) fn reject_late_system_messages(
    source_messages: &[Value],
) -> Result<(), AnthropicAdapterError> {
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
