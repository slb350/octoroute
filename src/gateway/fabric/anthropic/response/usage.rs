//! Anthropic-to-OpenAI token accounting.

use serde_json::{Map, Number, Value};

/// Anthropic token accounting, as far as the upstream actually reported it.
///
/// Every count is optional on purpose. Reporting a zero for a half the upstream
/// did not send tells a cost-tracking client that half was free, which is worse
/// than telling it nothing. A stream reports the two halves in different
/// events, so a partial report is the normal case rather than a malformed one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct AnthropicUsage {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_creation: Option<u64>,
}

/// Read Anthropic usage, or `None` when the object carries no count at all.
pub(super) fn read_usage(usage: Option<&Value>) -> Option<AnthropicUsage> {
    let usage = usage.and_then(Value::as_object)?;
    let field = |name: &str| usage.get(name).and_then(Value::as_u64);
    let parsed = AnthropicUsage {
        input: field("input_tokens"),
        output: field("output_tokens"),
        cache_read: field("cache_read_input_tokens"),
        cache_creation: field("cache_creation_input_tokens"),
    };
    parsed.is_reported().then_some(parsed)
}

impl AnthropicUsage {
    const fn is_reported(self) -> bool {
        self.input.is_some()
            || self.output.is_some()
            || self.cache_read.is_some()
            || self.cache_creation.is_some()
    }

    /// Prefer this report's counts, filling each gap from an earlier one.
    pub(super) fn merge(self, earlier: Self) -> Self {
        Self {
            input: self.input.or(earlier.input),
            output: self.output.or(earlier.output),
            cache_read: self.cache_read.or(earlier.cache_read),
            cache_creation: self.cache_creation.or(earlier.cache_creation),
        }
    }

    /// The OpenAI `prompt_tokens` equivalent.
    ///
    /// Anthropic reports cache reads and cache writes beside `input_tokens`
    /// rather than inside it, while OpenAI's `prompt_tokens` is the inclusive
    /// total. Dropping the cache halves undercounts a cached prompt by exactly
    /// the tokens the caller was billed for.
    fn prompt_tokens(self) -> Option<u64> {
        (self.input.is_some() || self.cache_read.is_some() || self.cache_creation.is_some()).then(
            || {
                self.input
                    .unwrap_or_default()
                    .saturating_add(self.cache_read.unwrap_or_default())
                    .saturating_add(self.cache_creation.unwrap_or_default())
            },
        )
    }

    pub(super) fn into_open_ai(self) -> Value {
        let prompt = self.prompt_tokens();
        let mut usage = Map::new();
        if let Some(prompt) = prompt {
            usage.insert(
                "prompt_tokens".to_string(),
                Value::Number(Number::from(prompt)),
            );
        }
        if let Some(output) = self.output {
            usage.insert(
                "completion_tokens".to_string(),
                Value::Number(Number::from(output)),
            );
        }
        if let (Some(prompt), Some(output)) = (prompt, self.output) {
            usage.insert(
                "total_tokens".to_string(),
                Value::Number(Number::from(prompt.saturating_add(output))),
            );
        }
        let mut details = Map::new();
        if let Some(cache_read) = self.cache_read {
            details.insert(
                "cached_tokens".to_string(),
                Value::Number(Number::from(cache_read)),
            );
        }
        if let Some(cache_creation) = self.cache_creation {
            details.insert(
                "cache_creation_tokens".to_string(),
                Value::Number(Number::from(cache_creation)),
            );
        }
        if !details.is_empty() {
            usage.insert("prompt_tokens_details".to_string(), Value::Object(details));
        }
        Value::Object(usage)
    }
}
