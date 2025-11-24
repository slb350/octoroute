//! Comprehensive tier validation tests
//!
//! Tests for invalid tier string handling at deserialization time.
//! Addresses PR #4 Test Coverage Gaps 1, 2, and 3.

use octoroute::config::Config;
use octoroute::models::selector::ModelSelector;
use octoroute::router::llm_based::LlmBasedRouter;
use std::sync::Arc;

/// Helper to create test metrics
#[allow(dead_code)]
fn test_metrics() -> Arc<octoroute::metrics::Metrics> {
    Arc::new(octoroute::metrics::Metrics::new().expect("should create metrics"))
}

/// Test that uppercase "FAST" is rejected during deserialization
#[test]
fn test_uppercase_fast_tier_rejected() {
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_tier = "FAST"
"#;

    let result: Result<Config, _> = toml::from_str(config_toml);

    assert!(
        result.is_err(),
        "Config with uppercase 'FAST' should be rejected at deserialization"
    );

    let err = result.unwrap_err();
    let err_msg = err.to_string();

    // Should provide helpful message suggesting lowercase
    assert!(
        err_msg.to_lowercase().contains("fast")
            && (err_msg.contains("lowercase") || err_msg.contains("did you mean")),
        "Error should suggest lowercase 'fast', got: {}",
        err_msg
    );
}

/// Test that typo "fasst" is rejected with helpful message
#[test]
fn test_typo_fasst_tier_rejected() {
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_tier = "fasst"
"#;

    let result: Result<Config, _> = toml::from_str(config_toml);

    assert!(
        result.is_err(),
        "Config with typo 'fasst' should be rejected at deserialization"
    );

    let err = result.unwrap_err();
    let err_msg = err.to_string();

    // Should indicate invalid value and list valid options
    assert!(
        err_msg.contains("fasst") || err_msg.contains("invalid"),
        "Error should indicate invalid value, got: {}",
        err_msg
    );
}

/// Test that empty string "" is rejected
#[test]
fn test_empty_string_tier_rejected() {
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_tier = ""
"#;

    let result: Result<Config, _> = toml::from_str(config_toml);

    assert!(
        result.is_err(),
        "Config with empty string router_tier should be rejected at deserialization"
    );

    let err = result.unwrap_err();
    let err_msg = err.to_string();

    // Should indicate invalid value
    assert!(
        err_msg.contains("invalid") || err_msg.contains("valid values"),
        "Error should indicate invalid value, got: {}",
        err_msg
    );
}

/// Test that mixed case "Balanced" is rejected
#[test]
fn test_mixed_case_balanced_tier_rejected() {
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_tier = "Balanced"
"#;

    let result: Result<Config, _> = toml::from_str(config_toml);

    assert!(
        result.is_err(),
        "Config with mixed case 'Balanced' should be rejected at deserialization"
    );

    let err = result.unwrap_err();
    let err_msg = err.to_string();

    // Should suggest lowercase
    assert!(
        err_msg.to_lowercase().contains("balanced")
            && (err_msg.contains("lowercase") || err_msg.contains("did you mean")),
        "Error should suggest lowercase 'balanced', got: {}",
        err_msg
    );
}

/// Test that all valid lowercase tier strings are accepted
#[test]
fn test_valid_tier_strings_accepted() {
    for tier in &["fast", "balanced", "deep"] {
        let config_toml = format!(
            r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_tier = "{}"
"#,
            tier
        );

        let result: Result<Config, _> = toml::from_str(&config_toml);

        assert!(
            result.is_ok(),
            "Valid tier '{}' should be accepted, got error: {:?}",
            tier,
            result.err()
        );
    }
}

//
// Gap 2: Rule Strategy Validation
//

/// Test that Rule strategy still validates router_tier
///
/// Even though Rule strategy doesn't use router_tier for LLM routing,
/// the field must still be valid to prevent configuration errors.
#[test]
fn test_rule_strategy_with_invalid_router_tier_rejected() {
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "rule"
default_importance = "normal"
router_tier = "INVALID"
"#;

    let result: Result<Config, _> = toml::from_str(config_toml);

    assert!(
        result.is_err(),
        "Rule strategy with invalid router_tier should be rejected at deserialization"
    );

    let err = result.unwrap_err();
    let err_msg = err.to_string();

    assert!(
        err_msg.contains("INVALID")
            || err_msg.contains("invalid")
            || err_msg.contains("valid values"),
        "Error should indicate invalid router_tier value, got: {}",
        err_msg
    );
}

/// Test that all strategies reject invalid router_tier
#[test]
fn test_all_strategies_validate_router_tier() {
    for strategy in &["rule", "llm", "hybrid"] {
        let config_toml = format!(
            r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "{}"
default_importance = "normal"
router_tier = "UPPERCASE_INVALID"
"#,
            strategy
        );

        let result: Result<Config, _> = toml::from_str(&config_toml);

        assert!(
            result.is_err(),
            "Strategy '{}' should reject invalid router_tier at deserialization",
            strategy
        );
    }
}

//
// Gap 3: Concurrent Tier Isolation
//

/// Test that concurrent routers using different tiers don't interfere
///
/// Spawns three LLM routers concurrently (Fast, Balanced, Deep) and verifies
/// they can be created and operate independently without tier conflicts.
#[tokio::test]
async fn test_concurrent_tier_isolation() {
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "llm"
default_importance = "normal"
router_tier = "balanced"
"#;

    let config: Config = toml::from_str(config_toml).expect("Config should parse");
    let config = Arc::new(config);
    let metrics = Arc::new(octoroute::metrics::Metrics::new().unwrap());
    let selector = Arc::new(ModelSelector::new(config.clone(), metrics.clone()));

    // Spawn 3 concurrent tasks, each creating a router with different tier
    let handles = vec![
        tokio::spawn({
            let selector = selector.clone();
            let metrics = metrics.clone();
            async move {
                let router = LlmBasedRouter::new(
                    selector,
                    octoroute::router::TargetModel::Fast,
                    10,
                    metrics,
                );
                assert!(
                    router.is_ok(),
                    "Fast tier router should be created successfully"
                );
                ("fast", router.unwrap())
            }
        }),
        tokio::spawn({
            let selector = selector.clone();
            let metrics = metrics.clone();
            async move {
                let router = LlmBasedRouter::new(
                    selector,
                    octoroute::router::TargetModel::Balanced,
                    10,
                    metrics,
                );
                assert!(
                    router.is_ok(),
                    "Balanced tier router should be created successfully"
                );
                ("balanced", router.unwrap())
            }
        }),
        tokio::spawn({
            let selector = selector.clone();
            let metrics = metrics.clone();
            async move {
                let router = LlmBasedRouter::new(
                    selector,
                    octoroute::router::TargetModel::Deep,
                    10,
                    metrics,
                );
                assert!(
                    router.is_ok(),
                    "Deep tier router should be created successfully"
                );
                ("deep", router.unwrap())
            }
        }),
    ];

    // Wait for all routers to be created
    let mut results = Vec::new();
    for handle in handles {
        let (tier_name, _router) = handle.await.expect("Task should complete successfully");
        results.push(tier_name);
    }

    // Verify all three routers were created
    assert_eq!(results.len(), 3, "All 3 routers should be created");
    assert!(results.contains(&"fast"), "Fast router should be present");
    assert!(
        results.contains(&"balanced"),
        "Balanced router should be present"
    );
    assert!(results.contains(&"deep"), "Deep router should be present");
}
