//! Tests for configurable router query timeouts per tier
//!
//! Verifies that router query timeouts can be configured separately for each tier
//! (fast/balanced/deep) to accommodate different model response times.
//!
//! ## Rationale (from PR #4 Review - Issue #2)
//!
//! The hardcoded 10-second timeout may be insufficient for Deep tier (120B models)
//! routing under load. Larger models need more time to analyze routing decisions.
//!
//! With configurable timeouts:
//! - Fast tier: 5s (small 8B models, quick routing)
//! - Balanced tier: 10s (medium 30B models, default)
//! - Deep tier: 20s (large 120B models, complex routing)

use octoroute::config::Config;

/// RED PHASE: Test that router timeouts can be configured per tier
///
/// This test will FAIL because current implementation:
/// - No router_timeouts field in RoutingConfig
/// - Timeout is hardcoded at 10s in LlmBasedRouter
///
/// Expected after implementation:
/// - RoutingConfig has router_timeouts field with fast/balanced/deep timeouts
/// - Config can be parsed with custom timeout values
/// - Defaults are provided if not specified
#[test]
fn test_config_parses_router_timeouts() {
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:1234/v1"
max_tokens = 2048

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "llm"
router_tier = "balanced"

[routing.router_timeouts]
fast = 5
balanced = 10
deep = 20
"#;

    let config: Config = toml::from_str(config_toml).expect("should parse config");

    // Verify router timeouts are parsed
    assert_eq!(
        config
            .routing
            .router_timeout_for_tier(octoroute::router::TargetModel::Fast),
        5,
        "Fast tier should have 5s timeout"
    );
    assert_eq!(
        config
            .routing
            .router_timeout_for_tier(octoroute::router::TargetModel::Balanced),
        10,
        "Balanced tier should have 10s timeout"
    );
    assert_eq!(
        config
            .routing
            .router_timeout_for_tier(octoroute::router::TargetModel::Deep),
        20,
        "Deep tier should have 20s timeout"
    );
}

/// Test that default timeouts are provided if not specified
///
/// Ensures backward compatibility - configs without router_timeouts
/// should get sensible defaults.
#[test]
fn test_config_uses_default_router_timeouts() {
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:1234/v1"
max_tokens = 2048

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "llm"
router_tier = "balanced"
"#;

    let config: Config = toml::from_str(config_toml).expect("should parse config");

    // Should use default timeouts
    assert_eq!(
        config
            .routing
            .router_timeout_for_tier(octoroute::router::TargetModel::Fast),
        10,
        "Fast tier should default to 10s"
    );
    assert_eq!(
        config
            .routing
            .router_timeout_for_tier(octoroute::router::TargetModel::Balanced),
        10,
        "Balanced tier should default to 10s"
    );
    assert_eq!(
        config
            .routing
            .router_timeout_for_tier(octoroute::router::TargetModel::Deep),
        10,
        "Deep tier should default to 10s"
    );
}

/// Test that partial timeout configuration is rejected by TOML parser
///
/// Ensures TOML parsing catches incomplete timeout specifications
/// (e.g., specifying only fast timeout but not balanced/deep).
/// This is handled by TOML parser, not validation.
#[test]
fn test_config_rejects_partial_router_timeouts() {
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:1234/v1"
max_tokens = 2048

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "llm"
router_tier = "balanced"

[routing.router_timeouts]
fast = 5
# Missing balanced and deep - TOML will reject this
"#;

    // TOML parser rejects incomplete structs (all fields must be present)
    let result = toml::from_str::<Config>(config_toml);
    assert!(
        result.is_err(),
        "Config with partial router_timeouts should be rejected by TOML parser"
    );
}

/// Test that zero or negative timeouts are rejected
///
/// Ensures validation catches invalid timeout values.
#[test]
fn test_config_rejects_invalid_router_timeouts() {
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:1234/v1"
max_tokens = 2048

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "llm"
router_tier = "balanced"

[routing.router_timeouts]
fast = 0
balanced = 10
deep = 20
"#;

    let config = toml::from_str::<Config>(config_toml).expect("should parse");
    let result = config.validate();
    assert!(
        result.is_err(),
        "Config with zero timeout should be rejected during validation"
    );
}
