//! Response size limit tests
//!
//! Note: The streaming code (try_router_query) enforces MAX_ROUTER_RESPONSE
//! and returns an error if exceeded. These tests verify that the parsing
//! function itself can handle long strings if they somehow bypass that check.

use super::*;

#[test]
fn test_parse_routing_decision_handles_long_responses() {
    // Streaming code enforces 1KB limit, but parser should handle long strings
    // gracefully if they somehow reach it (e.g., in tests or edge cases)
    // Use spaces to ensure word boundaries are respected
    let long_response = format!(
        "{} BALANCED {}",
        "x".repeat(500), // 500 chars before (with space)
        "y".repeat(500)  // 500 chars after (with space)
    );

    let result = LlmBasedRouter::parse_routing_decision(&long_response);
    assert!(
        result.is_ok(),
        "Parser should handle long responses with keywords at word boundaries"
    );
    assert_eq!(result.unwrap(), TargetModel::Balanced);
}

#[test]
fn test_parse_routing_decision_handles_extreme_length() {
    // Parser should not crash on extremely long strings (even if streaming
    // code would have rejected them at 1KB limit)
    // Use space to ensure word boundary is respected
    let extreme_response = format!("FAST {}", "x".repeat(1_000_000));

    let result = LlmBasedRouter::parse_routing_decision(&extreme_response);
    assert!(result.is_ok(), "Parser should not crash on extreme length");
    assert_eq!(result.unwrap(), TargetModel::Fast);
}
