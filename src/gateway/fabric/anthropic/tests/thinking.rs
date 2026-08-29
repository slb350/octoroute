//! Thinking budgets, reasoning controls, and sampling parameters.

use super::super::AnthropicAdapterError;
use super::{build_request, chat, config, provider_with, request, translate};
use crate::gateway::fabric::{ProviderConfig, ReasoningEffort};
use serde_json::{Value, json};

/// `max_tokens` is the total for thinking plus the answer, so the budget has to
/// leave a usable answer allowance rather than consuming all but one token.
#[test]
fn thinking_budget_reserves_an_answer_allowance() {
    for (max_tokens, effort) in [
        (2_048_u64, "high"),
        (4_096, "high"),
        (8_192, "high"),
        (16_384, "high"),
        (4_096, "medium"),
        (32_768, "xhigh"),
    ] {
        let body = translate(chat(
            json!({"max_tokens": max_tokens, "reasoning_effort": effort}),
        ));
        let budget = body["thinking"]["budget_tokens"]
            .as_u64()
            .expect("thinking budget");
        assert!(
            budget >= 1_024,
            "budget {budget} is below Anthropic's minimum for max_tokens {max_tokens}"
        );
        assert!(
            max_tokens - budget >= budget,
            "budget {budget} leaves only {} answer tokens of {max_tokens}",
            max_tokens - budget
        );
    }
}

/// Below twice Anthropic's minimum budget no thinking allocation both clears the
/// minimum and leaves an answer, so an explicit reasoning request is incompatible.
#[test]
fn unaffordable_thinking_budget_fails_closed() {
    let config = config();
    for max_tokens in [1_024_u64, 1_025, 2_047] {
        let error = build_request(
            &config.providers["kimi"],
            &request(chat(
                json!({"max_tokens": max_tokens, "reasoning_effort": "high"}),
            )),
        )
        .expect_err("unaffordable reasoning must not be silently disabled");
        assert!(
            error.is_incompatible(),
            "max_tokens {max_tokens} must fail closed"
        );
    }
}

/// Thinking is opt-in. Neither the shipped `kimi` provider nor the route default
/// asks for it, so an ordinary request must not arrive with thinking enabled.
#[test]
fn thinking_is_not_enabled_without_an_explicit_reasoning_control() {
    let body = translate(chat(json!({"max_tokens": 200_000})));
    assert!(body.get("thinking").is_none());
}

/// Anthropic rejects `temperature`, `top_p`, and `top_k` alongside thinking,
/// so a request that asks for both is incompatible rather than silently changed.
#[test]
fn sampling_controls_with_thinking_fail_closed() {
    let config = config();
    for (field, value) in [
        ("temperature", json!(0.7)),
        ("top_p", json!(0.9)),
        ("top_k", json!(40)),
    ] {
        let error = build_request(
            &config.providers["kimi"],
            &request(chat(json!({
                "max_tokens": 8_192,
                "reasoning_effort": "high",
                (field): value
            }))),
        )
        .expect_err("sampling plus thinking must fail closed");
        assert!(
            error.is_incompatible(),
            "{field} must not be silently dropped"
        );
    }
}

#[test]
fn caller_sampling_survives_when_thinking_is_disabled() {
    let body = translate(chat(json!({"temperature": 0.7, "top_p": 0.9})));
    assert!(body.get("thinking").is_none());
    assert_eq!(body["temperature"], 0.7);
    assert_eq!(body["top_p"], 0.9);
}

#[test]
fn malformed_controls_fail_closed() {
    let config = config();
    for malformed in [
        json!({"n": "1"}),
        json!({"response_format": "text"}),
        json!({"max_completion_tokens": "many"}),
        json!({"reasoning_effort": 1}),
    ] {
        let error = build_request(&config.providers["kimi"], &request(chat(malformed)))
            .expect_err("malformed control must fail closed");
        assert!(error.is_incompatible());
    }
}

/// `reasoning: {"enabled": true}` is documented as reasoning at medium effort.
/// Reading only `reasoning.effort` produces a non-thinking request and discards
/// the caller's reasoning control.
#[test]
fn reasoning_enabled_asks_for_medium_thinking() {
    let body = translate(chat(json!({
        "max_tokens": 100_000,
        "reasoning": {"enabled": true}
    })));
    assert_eq!(
        body["thinking"],
        json!({"type": "enabled", "budget_tokens": 4_096}),
        "`enabled: true` is medium effort"
    );
}

/// `reasoning.max_tokens` is Anthropic's own control, so it maps straight onto
/// the thinking budget - still subject to the half-of-`max_tokens` ceiling and
/// Anthropic's 1024-token floor.
#[test]
fn reasoning_max_tokens_maps_onto_the_thinking_budget() {
    for (max_tokens, requested, expected) in
        [(8_192_u64, 2_000_u64, 2_000_u64), (8_192, 100_000, 4_096)]
    {
        let body = translate(chat(json!({
            "max_tokens": max_tokens,
            "reasoning": {"max_tokens": requested}
        })));
        assert_eq!(
            body.get("thinking")
                .and_then(|thinking| thinking["budget_tokens"].as_u64()),
            Some(expected),
            "reasoning.max_tokens {requested} against max_tokens {max_tokens}"
        );
    }

    let error = build_request(
        &config().providers["kimi"],
        &request(chat(json!({
            "max_tokens": 8_192,
            "reasoning": {"max_tokens": 500}
        }))),
    )
    .expect_err("a sub-minimum explicit budget must fail closed");
    assert!(error.is_incompatible());

    // Zero must be rejected by the nested parser itself, not rescued later by
    // the affordability check: only the parser's label proves zero never
    // became a budget.
    let error = build_request(
        &config().providers["kimi"],
        &request(chat(json!({
            "max_tokens": 8_192,
            "reasoning": {"max_tokens": 0}
        }))),
    )
    .expect_err("a zero explicit budget must fail closed");
    assert!(
        matches!(
            error,
            AnthropicAdapterError::Incompatible("reasoning max_tokens")
        ),
        "zero must be rejected as a malformed reasoning.max_tokens, got {error:?}"
    );
}

/// A provider-level `reasoning_effort` is the fallback when the caller names
/// none, and an explicit `reasoning: {"enabled": false}` overrides it.
#[test]
fn provider_reasoning_effort_applies_only_without_a_caller_control() {
    let provider = provider_with(Some(ReasoningEffort::High));
    let default = build_request(&provider, &request(chat(json!({"max_tokens": 100_000}))))
        .expect("Anthropic request");
    let default: Value = serde_json::from_slice(&default.body).expect("translated JSON");
    assert_eq!(default["thinking"]["budget_tokens"], 16_384);

    let disabled = build_request(
        &provider,
        &request(chat(
            json!({"max_tokens": 100_000, "reasoning": {"enabled": false}}),
        )),
    )
    .expect("Anthropic request");
    let disabled: Value = serde_json::from_slice(&disabled.body).expect("translated JSON");
    assert!(
        disabled.get("thinking").is_none(),
        "an explicit `enabled: false` must beat the provider default"
    );
}

/// Controls inside the `reasoning` object that Anthropic cannot express, or
/// that OpenRouter documents as mutually exclusive, fail closed.
#[test]
fn contradictory_or_unmappable_reasoning_controls_fail_closed() {
    let config = config();
    for reasoning in [
        // Reasoning the caller wants hidden; Anthropic always returns thinking.
        json!({"effort": "high", "exclude": true}),
        // Documented as one or the other, never both.
        json!({"effort": "high", "max_tokens": 2_000}),
        json!({"enabled": false, "effort": "high"}),
        json!({"effort": "minimal"}),
        json!({"enabled": "yes"}),
        json!({"max_tokens": 0}),
        json!("high"),
    ] {
        let error = build_request(
            &config.providers["kimi"],
            &request(chat(json!({"reasoning": reasoning}))),
        )
        .expect_err("unmappable reasoning control must fail closed");
        assert!(error.is_incompatible());
    }
}

/// Anthropic caps `temperature` and `top_p` at 1 and takes `top_k` as an
/// integer count. An `is_number` check forwards values the API rejects, which
/// the client only learns about after the route has committed.
#[test]
fn out_of_range_sampling_values_fail_closed() {
    let config = config();
    for sampling in [
        json!({"temperature": 1.5}),
        json!({"temperature": -0.1}),
        json!({"top_p": 1.2}),
        json!({"top_p": -1}),
        json!({"top_k": 3.5}),
        json!({"top_k": -1}),
    ] {
        let error = build_request(&config.providers["kimi"], &request(chat(sampling.clone())))
            .expect_err("out-of-range sampling value must fail closed");
        assert!(error.is_incompatible(), "{sampling} must be incompatible");
    }
    // The accepted boundaries still pass.
    let body = translate(chat(json!({"temperature": 1.0, "top_p": 0.0, "top_k": 0})));
    assert_eq!(body["temperature"], 1.0);
    assert_eq!(body["top_p"], 0.0);
    assert_eq!(body["top_k"], 0);
}

/// Anthropic's `max_tokens` must be at least 1, so a zero limit is not a
/// smaller budget - it is a 400 the client sees only after the route has
/// committed to this provider.
#[test]
fn a_zero_token_limit_fails_closed() {
    let config = config();
    for limit in [
        json!({"max_tokens": 0}),
        json!({"max_completion_tokens": 0}),
    ] {
        let error = build_request(&config.providers["kimi"], &request(chat(limit.clone())))
            .expect_err("a zero token limit must fail closed");
        assert!(error.is_incompatible(), "{limit} must be incompatible");
    }
    assert_eq!(
        translate(chat(json!({"max_tokens": 1})))["max_tokens"],
        1,
        "one token is a usable limit"
    );
}

/// Each mapped effort level has its own budget. An effort that stops resolving
/// does not fail the request - it falls back to the provider default or to no
/// thinking at all, so a lost level looks like a working request.
#[test]
fn every_mapped_reasoning_effort_has_its_own_budget() {
    for (effort, budget) in [
        ("low", 1_024),
        ("medium", 4_096),
        ("high", 16_384),
        ("xhigh", 32_768),
    ] {
        let body = translate(chat(
            json!({"max_tokens": 200_000, "reasoning_effort": effort}),
        ));
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": budget}),
            "reasoning_effort {effort}"
        );
    }
}

/// `stop` becomes Anthropic's `stop_sequences`. Dropping it returns text the
/// caller asked to be cut off, and forwarding a non-string sequence buys a 400.
#[test]
fn stop_sequences_are_translated_and_unmappable_ones_fail_closed() {
    assert_eq!(
        translate(chat(json!({"stop": "END"})))["stop_sequences"],
        json!(["END"]),
        "a bare string is a one-element sequence"
    );
    assert_eq!(
        translate(chat(json!({"stop": ["END", "STOP"]})))["stop_sequences"],
        json!(["END", "STOP"])
    );
    for absent in [json!({}), json!({"stop": Value::Null})] {
        assert!(
            translate(chat(absent.clone()))
                .get("stop_sequences")
                .is_none(),
            "{absent} must not invent a stop sequence"
        );
    }

    let config = config();
    for stop in [
        json!({"stop": ["END", 4]}),
        json!({"stop": 4}),
        json!({"stop": {"text": "END"}}),
    ] {
        let error = build_request(&config.providers["kimi"], &request(chat(stop.clone())))
            .expect_err("an unmappable stop must fail closed");
        assert!(error.is_incompatible(), "{stop} must be incompatible");
    }
}

/// A provider-configured `temperature` gets the same range check as a
/// caller-supplied one. Config validation only rejects a non-finite value, so
/// an out-of-range default would otherwise reach Anthropic and be refused.
#[test]
fn a_provider_configured_temperature_is_range_checked() {
    let with_temperature = |temperature: f64| {
        let mut provider = config().providers["kimi"].clone();
        provider.temperature = Some(temperature);
        provider
    };
    let translated = |provider: &ProviderConfig, value: Value| {
        let built = build_request(provider, &request(chat(value))).expect("Anthropic request");
        serde_json::from_slice::<Value>(&built.body).expect("translated JSON")
    };

    assert_eq!(
        translated(&with_temperature(0.3), json!({}))["temperature"],
        0.3,
        "an in-range provider default is forwarded"
    );
    for temperature in [1.5, -0.1] {
        let error = build_request(&with_temperature(temperature), &request(chat(json!({}))))
            .expect_err("an out-of-range provider temperature must fail closed");
        assert!(error.is_incompatible(), "temperature {temperature}");
    }
    assert_eq!(
        translated(&with_temperature(1.5), json!({"temperature": 0.2}))["temperature"],
        0.2,
        "the caller's own value replaces the provider default"
    );
}
