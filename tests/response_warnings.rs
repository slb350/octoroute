//! Tests for warning propagation in routing responses
//!
//! Verifies that health tracking failures are surfaced to users via warnings
//! in the response, addressing CRITICAL-1 from PR #4 review.
//!
//! ## Background
//!
//! Health tracking failures (mark_success/mark_failure errors) are currently
//! logged but requests succeed silently. This causes degraded health tracking
//! without user awareness, leading to poor endpoint recovery.
//!
//! ## Solution
//!
//! Add `warnings: Vec<String>` field to RoutingDecision and ChatResponse to
//! surface health tracking failures to users while still completing requests.

use octoroute::router::{RoutingDecision, RoutingStrategy, TargetModel};

#[test]
fn test_routing_decision_has_warnings_field() {
    // RED: RoutingDecision should have a warnings field
    //
    // This test will fail because RoutingDecision doesn't have a warnings field yet.
    // After implementation, RoutingDecision::new() should initialize warnings to empty Vec.

    let decision = RoutingDecision::new(TargetModel::Fast, RoutingStrategy::Rule);

    // Test that warnings field exists and is accessible
    let warnings = decision.warnings();
    assert!(
        warnings.is_empty(),
        "New RoutingDecision should have empty warnings"
    );
}

#[test]
fn test_routing_decision_with_warnings() {
    // RED: RoutingDecision should support adding warnings
    //
    // This test will fail because RoutingDecision::with_warnings() doesn't exist yet.
    // After implementation, should be able to create a decision with warnings.

    let decision = RoutingDecision::new(TargetModel::Balanced, RoutingStrategy::Llm)
        .with_warning("Health tracking failure: UnknownEndpoint 'balanced-1'".to_string());

    let warnings = decision.warnings();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("Health tracking failure"));
    assert!(warnings[0].contains("UnknownEndpoint"));
    assert!(warnings[0].contains("balanced-1"));
}

#[test]
fn test_routing_decision_with_multiple_warnings() {
    // RED: RoutingDecision should support multiple warnings
    //
    // This test verifies that multiple warnings can be accumulated.

    let mut decision = RoutingDecision::new(TargetModel::Deep, RoutingStrategy::Llm);
    decision = decision.with_warning("First warning".to_string());
    decision = decision.with_warning("Second warning".to_string());

    let warnings = decision.warnings();
    assert_eq!(warnings.len(), 2);
    assert_eq!(warnings[0], "First warning");
    assert_eq!(warnings[1], "Second warning");
}

#[test]
fn test_routing_decision_serializes_warnings() {
    // RED: RoutingDecision should serialize with warnings field
    //
    // This test will fail because RoutingDecision doesn't implement Serialize yet.
    // After implementation, warnings should appear in JSON output.

    let decision = RoutingDecision::new(TargetModel::Fast, RoutingStrategy::Rule)
        .with_warning("Test warning".to_string());

    let json = serde_json::to_string(&decision).expect("should serialize");
    assert!(json.contains("warnings"));
    assert!(json.contains("Test warning"));
}

#[test]
fn test_routing_decision_omits_empty_warnings_from_json() {
    // RED: RoutingDecision should skip serializing warnings if empty
    //
    // This test verifies that empty warnings arrays are omitted from JSON
    // (using #[serde(skip_serializing_if = "Vec::is_empty")]).

    let decision = RoutingDecision::new(TargetModel::Fast, RoutingStrategy::Rule);

    let json = serde_json::to_string(&decision).expect("should serialize");
    assert!(
        !json.contains("warnings"),
        "Empty warnings should be omitted from JSON"
    );
}

#[test]
fn test_chat_response_includes_warnings() {
    // RED: ChatResponse should have a warnings field
    //
    // This test will fail because ChatResponse doesn't have a warnings field yet.
    // After implementation, ChatResponse should include warnings from RoutingDecision.

    use octoroute::config::ModelEndpoint;

    let toml = r#"
name = "test-model"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1
"#;
    let endpoint: ModelEndpoint = toml::from_str(toml).expect("should parse endpoint");

    let warnings = vec!["Health tracking degraded".to_string()];
    let response = octoroute::handlers::chat::ChatResponse::new_with_warnings(
        "Response text".to_string(),
        &endpoint,
        TargetModel::Balanced,
        RoutingStrategy::Llm,
        warnings.clone(),
    );

    // Verify warnings are accessible
    assert_eq!(response.warnings(), &warnings);
}

#[test]
fn test_chat_response_serializes_warnings() {
    // RED: ChatResponse should serialize warnings in JSON
    //
    // This test verifies that warnings appear in the JSON response sent to users.

    use octoroute::config::ModelEndpoint;

    let toml = r#"
name = "test-model"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1
"#;
    let endpoint: ModelEndpoint = toml::from_str(toml).expect("should parse endpoint");

    let warnings = vec![
        "Health tracking failure: UnknownEndpoint 'balanced-1'".to_string(),
        "Endpoint recovery may be impaired".to_string(),
    ];
    let response = octoroute::handlers::chat::ChatResponse::new_with_warnings(
        "Response text".to_string(),
        &endpoint,
        TargetModel::Balanced,
        RoutingStrategy::Llm,
        warnings,
    );

    let json = serde_json::to_string(&response).expect("should serialize");
    assert!(json.contains("warnings"));
    assert!(json.contains("Health tracking failure"));
    assert!(json.contains("UnknownEndpoint"));
    assert!(json.contains("balanced-1"));
}

#[test]
fn test_chat_response_omits_empty_warnings() {
    // RED: ChatResponse should omit warnings field when empty
    //
    // This test verifies that empty warnings arrays don't clutter the JSON response.

    use octoroute::config::ModelEndpoint;

    let toml = r#"
name = "test-model"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1
"#;
    let endpoint: ModelEndpoint = toml::from_str(toml).expect("should parse endpoint");

    let response = octoroute::handlers::chat::ChatResponse::new(
        "Response text".to_string(),
        &endpoint,
        TargetModel::Fast,
        RoutingStrategy::Rule,
    );

    let json = serde_json::to_string(&response).expect("should serialize");
    assert!(
        !json.contains("warnings"),
        "Empty warnings should be omitted from JSON, got: {}",
        json
    );
}
