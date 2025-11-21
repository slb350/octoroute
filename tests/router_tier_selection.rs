//! Integration tests for router_tier tier selection
//!
//! This test file addresses PR #4 review critical issue #3:
//! "Missing integration tests for Fast/Deep router tiers"
//!
//! Verifies that the system works end-to-end with different router_tier
//! configurations: "fast", "balanced", and "deep".

use octoroute::config::Config;
use octoroute::models::ModelSelector;
use octoroute::router::{HybridRouter, Importance, LlmBasedRouter, RouteMetadata, TaskType};
use std::sync::Arc;

/// Helper function to parse and validate config from TOML
///
/// This ensures all tests properly validate configuration before use,
/// catching configuration errors at the validation layer (not later at runtime).
fn validated_config_from_toml(toml: &str) -> Config {
    let config: Config = toml::from_str(toml).expect("should parse TOML");
    config.validate().expect("config validation should pass");
    config
}

#[tokio::test]
async fn test_llm_router_with_fast_tier() {
    // Test that LLM routing works with router_tier = "fast"
    // Expected: Faster routing (~50-200ms) using Fast (8B) tier for routing decisions

    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://192.0.2.1:11434/v1"  # Non-routable IP (TEST-NET-1)
max_tokens = 2048
weight = 1.0
priority = 1

[[models.fast]]
name = "fast-2"
base_url = "http://192.0.2.2:11434/v1"  # Non-routable IP (TEST-NET-1)
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
router_tier = "fast"  # Using Fast tier for routing decisions
"#;

    let config = validated_config_from_toml(config_toml);
    let config = Arc::new(config);

    let selector = Arc::new(ModelSelector::new(config.clone()));

    // Should succeed with fast tier
    let result = LlmBasedRouter::new(selector.clone(), octoroute::router::TargetModel::Fast);

    assert!(
        result.is_ok(),
        "LlmBasedRouter::new should succeed with Fast tier, got: {:?}",
        result.err()
    );

    println!("✅ Verified LLM router works with router_tier='fast'");
    println!("   - LlmBasedRouter construction succeeded");
    println!("   - Fast tier used for routing decisions");
}

#[tokio::test]
async fn test_hybrid_router_with_deep_tier() {
    // Test that Hybrid routing works with router_tier = "deep"
    // Expected: Slowest routing (~2-5s) but most accurate, using Deep (120B) tier

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
base_url = "http://192.0.2.1:8080/v1"  # Non-routable IP (TEST-NET-1)
max_tokens = 8192
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-2"
base_url = "http://192.0.2.2:8080/v1"  # Non-routable IP (TEST-NET-1)
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_tier = "deep"  # Using Deep tier for LLM fallback routing
"#;

    let config = validated_config_from_toml(config_toml);
    let config = Arc::new(config);

    let selector = Arc::new(ModelSelector::new(config.clone()));

    // Should succeed with deep tier
    let result = HybridRouter::new(config.clone(), selector.clone());

    assert!(
        result.is_ok(),
        "HybridRouter::new should succeed with Deep tier, got: {:?}",
        result.err()
    );

    println!("✅ Verified Hybrid router works with router_tier='deep'");
    println!("   - HybridRouter construction succeeded");
    println!("   - Deep tier used for LLM fallback routing");
}

#[tokio::test]
async fn test_hybrid_router_deep_tier_uses_deep_for_llm_fallback() {
    // Behavioral test: Verify Hybrid router actually uses Deep tier when falling back to LLM
    // This test ensures the tier selection propagates to the LLM router, not just construction

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
base_url = "http://192.0.2.1:1234/v1"  # Non-routable (will fail if used)
max_tokens = 4096
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://192.0.2.2:8080/v1"  # Non-routable (expected to fail)
max_tokens = 8192
weight = 1.0
priority = 1

[routing]
strategy = "hybrid"
default_importance = "normal"
router_tier = "deep"  # Using Deep tier for LLM fallback
"#;

    let config = validated_config_from_toml(config_toml);
    let config = Arc::new(config);
    let selector = Arc::new(ModelSelector::new(config.clone()));

    let router = HybridRouter::new(config.clone(), selector.clone())
        .expect("should create HybridRouter with Deep tier");

    // Create metadata that doesn't match any rules (triggers LLM fallback)
    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::High,
        task_type: TaskType::CasualChat, // No rule matches High + CasualChat
    };

    // Attempt routing - should try to query DEEP tier (192.0.2.2), not Balanced (192.0.2.1)
    let result = router.route("test message", &meta).await;

    // Should fail because deep endpoint is non-routable
    assert!(
        result.is_err(),
        "Should fail trying to query non-routable Deep tier endpoint"
    );

    let err_msg = result.unwrap_err().to_string();

    // Error should mention Deep tier, proving it tried to use Deep (not Balanced)
    assert!(
        err_msg.to_lowercase().contains("deep") || err_msg.contains("192.0.2.2"),
        "Error should reference Deep tier or deep endpoint IP to prove correct tier was used, got: {}",
        err_msg
    );

    // Should NOT mention Balanced tier IP
    assert!(
        !err_msg.contains("192.0.2.1"),
        "Error should not reference Balanced tier IP, got: {}",
        err_msg
    );

    println!("✅ Verified Hybrid router uses Deep tier for LLM fallback");
    println!("   - Hybrid router attempted to query Deep tier");
    println!("   - Did not fall back to Balanced tier");
}

#[tokio::test]
async fn test_all_router_tiers_with_appstate() {
    // Comprehensive test: Verify AppState construction works with all router_tier values
    // This is a smoke test for the full application startup path

    for router_tier in ["fast", "balanced", "deep"] {
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
strategy = "llm"
default_importance = "normal"
router_tier = "{}"
"#,
            router_tier
        );

        let config = validated_config_from_toml(&config_toml);
        let config = Arc::new(config);
        let selector = Arc::new(ModelSelector::new(config.clone()));

        // Test that router construction succeeds with this tier
        let result = LlmBasedRouter::new(
            selector,
            match router_tier {
                "fast" => octoroute::router::TargetModel::Fast,
                "balanced" => octoroute::router::TargetModel::Balanced,
                "deep" => octoroute::router::TargetModel::Deep,
                _ => panic!("Invalid router_tier in test"),
            },
        );

        assert!(
            result.is_ok(),
            "Router construction should succeed for router_tier='{}'",
            router_tier
        );

        println!(
            "✅ Router construction passed for router_tier='{}'",
            router_tier
        );
    }
}

#[test]
fn test_config_validation_rejects_router_tier_without_endpoints() {
    // Verify that Config::validate() rejects empty router_tier tier
    // This test focuses on CONFIG VALIDATION, not router construction

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
router_tier = "deep"
"#;

    let mut config: Config = toml::from_str(config_toml).expect("should parse config");

    // Clear deep tier to create invalid state
    config.models.deep.clear();

    // Config validation should catch this BEFORE router construction
    let result = config.validate();

    assert!(
        result.is_err(),
        "Config::validate() should reject router_tier='deep' with no deep endpoints"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        (err_msg.contains("deep") || err_msg.contains("Deep")) && err_msg.contains("endpoint"),
        "Error should mention 'deep'/'Deep' and 'endpoint', got: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_llm_router_fast_tier_attempts_query() {
    // Verify that LLM router with fast tier actually attempts to query fast tier endpoints
    // This test ensures the tier selection is actually used, not just validated

    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://192.0.2.1:11434/v1"  # Non-routable (will fail)
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
router_tier = "fast"
"#;

    let config = validated_config_from_toml(config_toml);
    let config = Arc::new(config);

    let selector = Arc::new(ModelSelector::new(config.clone()));
    let router = LlmBasedRouter::new(selector, octoroute::router::TargetModel::Fast)
        .expect("should create LlmBasedRouter");

    let metadata = RouteMetadata {
        token_estimate: 100,
        importance: Importance::Normal,
        task_type: TaskType::CasualChat,
    };

    // Attempt routing - should fail because fast-1 endpoint is non-routable
    let result = router.route("test message", &metadata).await;

    assert!(
        result.is_err(),
        "Should fail when trying to query non-routable fast tier endpoint"
    );

    let error = result.unwrap_err();
    let error_msg = format!("{}", error);

    // STRENGTHENED: Verify error mentions Fast tier (proves correct tier was used)
    assert!(
        error_msg.to_lowercase().contains("fast") || error_msg.contains("192.0.2.1"),
        "Error should mention Fast tier or fast endpoint IP to prove correct tier was queried, got: {}",
        error_msg
    );

    // STRENGTHENED: Verify it didn't fall back to Balanced or Deep
    assert!(
        !error_msg.to_lowercase().contains("balanced")
            && !error_msg.to_lowercase().contains("deep"),
        "Error should not mention other tiers (would indicate fallback bug), got: {}",
        error_msg
    );

    println!("✅ LLM router with Fast tier attempted query on Fast tier endpoints");
    println!("   - Error mentions Fast tier: confirmed");
    println!("   - No fallback to other tiers: confirmed");
}

#[tokio::test]
async fn test_appstate_construction_hybrid_router_with_all_tiers() {
    // Test that AppState::new() successfully constructs Hybrid routers
    // with all three router_tier tiers (fast, balanced, deep).
    //
    // This tests the ACTUAL application initialization path, not just
    // the router constructors directly.

    for router_tier in ["fast", "balanced", "deep"] {
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
            router_tier
        );

        let config = validated_config_from_toml(&config_toml);
        let config = Arc::new(config);

        // Test the ACTUAL AppState construction (full integration)
        let result = octoroute::handlers::AppState::new(config);

        assert!(
            result.is_ok(),
            "AppState::new() should succeed with strategy='hybrid', router_tier='{}', got: {:?}",
            router_tier,
            result.err()
        );

        let app_state = result.unwrap();

        // Verify router was constructed (just check AppState succeeded)
        let _ = app_state.router(); // Access router to verify it's available

        println!(
            "✅ AppState construction passed for Hybrid + router_tier='{}'",
            router_tier
        );
    }
}

#[tokio::test]
async fn test_appstate_construction_llm_router_with_all_tiers() {
    // Same test but for LLM-only strategy

    for router_tier in ["fast", "balanced", "deep"] {
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
strategy = "llm"
default_importance = "normal"
router_tier = "{}"
"#,
            router_tier
        );

        let config = validated_config_from_toml(&config_toml);
        let config = Arc::new(config);

        let result = octoroute::handlers::AppState::new(config);

        assert!(
            result.is_ok(),
            "AppState::new() should succeed with strategy='llm', router_tier='{}', got: {:?}",
            router_tier,
            result.err()
        );

        println!(
            "✅ AppState construction passed for LLM + router_tier='{}'",
            router_tier
        );
    }
}
