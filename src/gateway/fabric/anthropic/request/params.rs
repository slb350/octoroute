//! Token limits, reasoning controls, and sampling parameters.

use super::fields::reject_unknown_keys;
use crate::gateway::fabric::ReasoningEffort;
use crate::gateway::fabric::anthropic::AnthropicAdapterError;
use serde_json::{Map, Number, Value};

pub(super) fn requested_max_tokens(
    source: &Map<String, Value>,
) -> Result<Option<u32>, AnthropicAdapterError> {
    for field in ["max_completion_tokens", "max_tokens"] {
        let Some(value) = source.get(field).filter(|value| !value.is_null()) else {
            continue;
        };
        let value = value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or(AnthropicAdapterError::Incompatible("max_tokens"))?;
        return Ok(Some(value));
    }
    Ok(None)
}

/// What the caller's reasoning controls asked for, once translated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequestedThinking {
    /// An effort level, mapped onto a budget by [`thinking_budget`].
    Effort(ReasoningEffort),
    /// A token budget the caller named directly.
    Budget(u32),
    /// The caller explicitly turned reasoning off, overriding any provider
    /// default.
    Disabled,
}

pub(super) fn requested_thinking(
    source: &Map<String, Value>,
) -> Result<Option<RequestedThinking>, AnthropicAdapterError> {
    // The nested object is parsed even when `reasoning_effort` wins, so an
    // unmapped key inside it still fails the request closed.
    let nested = nested_reasoning(source)?;
    let direct = match source
        .get("reasoning_effort")
        .filter(|value| !value.is_null())
    {
        Some(Value::String(value)) => Some(
            parse_effort(value).ok_or(AnthropicAdapterError::Incompatible("reasoning effort"))?,
        ),
        Some(_) => return Err(AnthropicAdapterError::Incompatible("reasoning effort")),
        None => None,
    };
    Ok(direct.map(RequestedThinking::Effort).or(nested))
}

/// Every key OpenRouter documents on the `reasoning` object.
///
/// `context` and `mode` are deliberately absent: they are not in the documented
/// object, so they fail closed like any other unrecognized key.
const MAPPED_REASONING_FIELDS: [&str; 4] = ["effort", "max_tokens", "exclude", "enabled"];

/// Translate OpenRouter's unified `reasoning` object.
///
/// `effort` and `max_tokens` are documented as mutually exclusive; `enabled:
/// true` means reasoning at medium effort; `enabled: false` means no reasoning
/// at all. `exclude: true` asks for reasoning that is hidden from the response,
/// which the Anthropic protocol cannot express - thinking blocks are always
/// returned - so it fails closed rather than silently returning them.
fn nested_reasoning(
    source: &Map<String, Value>,
) -> Result<Option<RequestedThinking>, AnthropicAdapterError> {
    let Some(reasoning) = source.get("reasoning").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let reasoning = reasoning
        .as_object()
        .ok_or(AnthropicAdapterError::Incompatible("reasoning"))?;
    reject_unknown_keys(reasoning, &MAPPED_REASONING_FIELDS, "reasoning field")?;

    match reasoning.get("exclude").filter(|value| !value.is_null()) {
        None | Some(Value::Bool(false)) => {}
        Some(_) => return Err(AnthropicAdapterError::Incompatible("reasoning exclude")),
    }
    let effort = match reasoning.get("effort").filter(|value| !value.is_null()) {
        Some(Value::String(value)) => Some(
            parse_effort(value).ok_or(AnthropicAdapterError::Incompatible("reasoning effort"))?,
        ),
        Some(_) => return Err(AnthropicAdapterError::Incompatible("reasoning effort")),
        None => None,
    };
    let budget = match reasoning.get("max_tokens").filter(|value| !value.is_null()) {
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or(AnthropicAdapterError::Incompatible("reasoning max_tokens"))?,
        ),
        None => None,
    };
    let enabled = match reasoning.get("enabled").filter(|value| !value.is_null()) {
        Some(Value::Bool(enabled)) => Some(*enabled),
        Some(_) => return Err(AnthropicAdapterError::Incompatible("reasoning enabled")),
        None => None,
    };
    if effort.is_some() && budget.is_some() {
        return Err(AnthropicAdapterError::Incompatible("reasoning"));
    }
    if enabled == Some(false) {
        if effort.is_some() || budget.is_some() {
            return Err(AnthropicAdapterError::Incompatible("reasoning"));
        }
        return Ok(Some(RequestedThinking::Disabled));
    }
    Ok(match (effort, budget) {
        (Some(effort), _) => Some(RequestedThinking::Effort(effort)),
        (_, Some(budget)) => Some(RequestedThinking::Budget(budget)),
        // `enabled: true` alone is documented as medium effort.
        (None, None) => enabled.map(|_| RequestedThinking::Effort(ReasoningEffort::Medium)),
    })
}

fn parse_effort(value: &str) -> Option<ReasoningEffort> {
    match value {
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "xhigh" => Some(ReasoningEffort::Xhigh),
        _ => None,
    }
}

/// Anthropic's minimum accepted thinking budget.
const MIN_THINKING_BUDGET_TOKENS: u32 = 1_024;

/// Resolve the thinking budget, or `None` when thinking cannot be afforded.
///
/// `max_tokens` is the total for thinking plus the visible answer, so the budget
/// claims at most half of it and the remainder stays available for the answer.
/// Below Anthropic's 1024-token minimum there is no affordable budget. The
/// caller treats `None` as an incompatible translation rather than silently
/// disabling requested reasoning.
pub(super) fn thinking_budget(effort: ReasoningEffort, max_tokens: u32) -> Option<u32> {
    let desired = match effort {
        ReasoningEffort::Low => 1_024,
        ReasoningEffort::Medium => 4_096,
        ReasoningEffort::High => 16_384,
        ReasoningEffort::Xhigh => 32_768,
    };
    affordable_budget(desired, max_tokens)
}

/// Apply the half-of-`max_tokens` ceiling and the 1024-token floor.
pub(super) fn affordable_budget(desired: u32, max_tokens: u32) -> Option<u32> {
    let budget = desired.min(max_tokens / 2);
    (budget >= MIN_THINKING_BUDGET_TOKENS).then_some(budget)
}

/// Forward a sampling control Anthropic accepts only on the unit interval.
///
/// OpenAI accepts `temperature` up to 2 where Anthropic stops at 1, so an
/// `is_number` check forwards a value the API rejects - a 400 the client sees
/// only after the route has committed to this provider. Range-checking here
/// fails the step closed and lets the next one answer.
pub(super) fn copy_unit_interval(
    source: &Map<String, Value>,
    destination: &mut Map<String, Value>,
    field: &str,
) -> Result<(), AnthropicAdapterError> {
    let Some(value) = source.get(field).filter(|value| !value.is_null()) else {
        return Ok(());
    };
    if !value
        .as_f64()
        .is_some_and(|number| (0.0..=1.0).contains(&number))
    {
        return Err(AnthropicAdapterError::Incompatible("sampling value"));
    }
    destination.insert(field.to_string(), value.clone());
    Ok(())
}

/// Forward `top_k`, which Anthropic accepts only as a non-negative integer.
pub(super) fn copy_top_k(
    source: &Map<String, Value>,
    destination: &mut Map<String, Value>,
) -> Result<(), AnthropicAdapterError> {
    let Some(value) = source.get("top_k").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let top_k = value
        .as_u64()
        .ok_or(AnthropicAdapterError::Incompatible("sampling value"))?;
    destination.insert("top_k".to_string(), Value::Number(Number::from(top_k)));
    Ok(())
}

pub(super) fn copy_stop(
    source: &Map<String, Value>,
    destination: &mut Map<String, Value>,
) -> Result<(), AnthropicAdapterError> {
    let Some(stop) = source.get("stop").filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let stop_sequences = match stop {
        Value::String(_) => vec![stop.clone()],
        Value::Array(values) if values.iter().all(Value::is_string) => values.clone(),
        _ => return Err(AnthropicAdapterError::Incompatible("stop")),
    };
    destination.insert("stop_sequences".to_string(), Value::Array(stop_sequences));
    Ok(())
}
