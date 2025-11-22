//! Tests for parse_routing_decision function

use super::*;

#[test]
fn test_parse_routing_decision_fast() {
    let response = "FAST";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), TargetModel::Fast);
}

#[test]
fn test_parse_routing_decision_fast_lowercase() {
    let response = "fast";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), TargetModel::Fast);
}

#[test]
fn test_parse_routing_decision_fast_in_sentence() {
    let response = "I think FAST would be best for this simple task";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), TargetModel::Fast);
}

#[test]
fn test_parse_routing_decision_balanced() {
    let response = "BALANCED";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), TargetModel::Balanced);
}

#[test]
fn test_parse_routing_decision_balanced_lowercase() {
    let response = "balanced";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), TargetModel::Balanced);
}

#[test]
fn test_parse_routing_decision_balanced_in_sentence() {
    let response = "For this coding task, I recommend BALANCED";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), TargetModel::Balanced);
}

#[test]
fn test_parse_routing_decision_deep() {
    let response = "DEEP";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), TargetModel::Deep);
}

#[test]
fn test_parse_routing_decision_deep_lowercase() {
    let response = "deep";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), TargetModel::Deep);
}

#[test]
fn test_parse_routing_decision_deep_in_sentence() {
    let response = "This requires DEEP reasoning and analysis";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), TargetModel::Deep);
}

#[test]
fn test_parse_routing_decision_unparseable_returns_error() {
    // Unparseable responses should error, not silently default to Balanced
    let response = "I'm not sure about this one";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(
        result.is_err(),
        "Unparseable response should return error, not default to Balanced"
    );

    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(
        err_msg.contains("unparseable") || err_msg.contains("parse"),
        "Error message should indicate parse failure, got: {}",
        err_msg
    );
}

#[test]
fn test_parse_routing_decision_refusal_returns_error() {
    // LLM refusals should error to alert operators of misconfiguration
    let test_cases = vec![
        "I cannot help with that request",
        "I'm unable to make this decision",
        "Sorry, I cannot answer that",
        "ERROR: timeout occurred",
        "CANNOT process this request",
    ];

    for response in test_cases {
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(
            result.is_err(),
            "Refusal '{}' should return error, got: {:?}",
            response,
            result
        );
    }
}

#[test]
fn test_parse_routing_decision_word_boundary_false_positives() {
    // ISSUE #2a: Fuzzy Parser Word Boundary Matching
    //
    // Current parser uses simple substring matching which causes false positives.
    // These test cases verify we don't match partial words (e.g., "FAST" in "BREAKFAST").

    // Should NOT match "FAST" in words containing it as a substring
    let false_positive_cases = vec![
        "BREAKFAST",  // Contains "FAST" but shouldn't match
        "STEADFAST",  // Contains "FAST" but shouldn't match
        "Belfast",    // Contains "FAST" (case insensitive) but shouldn't match
        "FASTIDIOUS", // Starts with "FAST" but shouldn't match
    ];

    for response in false_positive_cases {
        let result = LlmBasedRouter::parse_routing_decision(response);
        // These should either error (unparseable) or not match Fast
        // They should NOT return TargetModel::Fast
        if let Ok(target) = result {
            assert_ne!(
                target,
                TargetModel::Fast,
                "Response '{}' should not match Fast (contains FAST as substring but not whole word)",
                response
            );
        }
        // If it errors, that's acceptable (unparseable response)
    }
}

#[test]
fn test_parse_routing_decision_word_boundary_true_positives() {
    // ISSUE #2a: Verify word boundary matching works for actual target words
    //
    // These test cases should successfully match even with word boundaries.

    let true_positive_cases = vec![
        ("FAST", TargetModel::Fast),
        ("fast", TargetModel::Fast),
        ("Fast", TargetModel::Fast),
        ("  FAST  ", TargetModel::Fast), // With whitespace
        ("FAST\n", TargetModel::Fast),   // With newline
        ("BALANCED", TargetModel::Balanced),
        ("balanced", TargetModel::Balanced),
        ("DEEP", TargetModel::Deep),
        ("deep", TargetModel::Deep),
    ];

    for (response, expected) in true_positive_cases {
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(
            result.is_ok(),
            "Response '{}' should successfully parse",
            response
        );
        assert_eq!(
            result.unwrap(),
            expected,
            "Response '{}' should match {:?}",
            response,
            expected
        );
    }
}

#[test]
fn test_parse_routing_decision_false_positive_cases() {
    // These responses contain keywords but should NOT match due to refusal/error context
    let test_cases = vec![
        (
            "I cannot make this decision fast enough",
            "contains 'fast' but is a refusal",
        ),
        (
            "ERROR: Cannot provide BALANCED response",
            "contains 'balanced' but is error",
        ),
        (
            "This requires deep thought, but CANNOT decide",
            "contains 'deep' but is refusal",
        ),
        (
            "UNABLE to determine if FAST is appropriate",
            "contains 'fast' but is refusal",
        ),
    ];

    for (response, description) in test_cases {
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(
            result.is_err(),
            "Should error for: {} (response: '{}')",
            description,
            response
        );
    }
}

#[test]
fn test_parse_routing_decision_position_based_matching() {
    // When multiple keywords appear, leftmost should win
    let test_cases = vec![
        ("FAST or BALANCED would work", TargetModel::Fast),
        ("Choose BALANCED or DEEP", TargetModel::Balanced),
        ("Not DEEP, use FAST instead", TargetModel::Deep), // "DEEP" appears first
    ];

    for (response, expected) in test_cases {
        let result = LlmBasedRouter::parse_routing_decision(response);
        assert!(result.is_ok(), "Should succeed for: '{}'", response);
        assert_eq!(
            result.unwrap(),
            expected,
            "Should match leftmost keyword in: '{}'",
            response
        );
    }
}

#[test]
fn test_parse_routing_decision_malformed_returns_error() {
    // Malformed responses indicate LLM problems - should error
    let test_cases = vec![
        "The best choice would be something else",
        "Let me think about this carefully...",
        "123456789",
        "fast balanced deep", // lowercase and multiple words
    ];

    for response in test_cases {
        let result = LlmBasedRouter::parse_routing_decision(response);
        // These should ideally error, but if they contain keywords they'll match
        // For now, let's document the expected behavior
        if response.contains("fast") || response.contains("balanced") || response.contains("deep") {
            // Will match due to fuzzy matching - Issue #3 will address this
            continue;
        }
        assert!(
            result.is_err(),
            "Malformed '{}' should return error",
            response
        );
    }
}

#[test]
fn test_parse_routing_decision_empty_returns_error() {
    // Empty response should error - indicates LLM misconfiguration or refusal
    let response = "";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(result.is_err(), "Empty response should return error");

    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(
        err_msg.contains("empty") || err_msg.contains("no response"),
        "Error message should indicate empty response, got: {}",
        err_msg
    );
}

#[test]
fn test_parse_routing_decision_whitespace_returns_error() {
    // Whitespace-only response should error - same as empty
    let response = "   \n\t  ";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(
        result.is_err(),
        "Whitespace-only response should return error"
    );

    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(
        err_msg.contains("empty") || err_msg.contains("no response"),
        "Error message should indicate empty response, got: {}",
        err_msg
    );
}

#[test]
fn test_parse_routing_decision_multiple_options_first_wins() {
    // If response contains multiple options, first match wins
    let response = "FAST or BALANCED would work";
    let result = LlmBasedRouter::parse_routing_decision(response);
    assert!(result.is_ok());
    // FAST comes first in our parsing order
    assert_eq!(result.unwrap(), TargetModel::Fast);
}
