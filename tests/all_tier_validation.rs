//! All-tier validation tests
//!
//! P1 Regression Fix: Restore validation requiring ALL tiers to have endpoints.
//!
//! ISSUE: Current validation only checks router_tier has endpoints, but RuleBasedRouter
//! and LlmBasedRouter can route to ANY tier (Fast/Balanced/Deep). If a tier is empty
//! and gets selected, it fails at runtime with "No available healthy endpoints".
//!
//! FIX: Require all three tiers to have at least one endpoint, regardless of strategy.

use octoroute::config::Config;
use std::str::FromStr;

#[test]
fn test_config_with_empty_fast_tier_rejected() {
    // P1 Regression: Rule router can route to Fast tier (CasualChat requests)
    // If Fast tier is empty, this fails at runtime instead of startup validation
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[models]
# Fast tier is EMPTY - no endpoints
# Rule router routes CasualChat here, will fail at runtime
fast = []

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
router_tier = "balanced"
"#;

    let result = Config::from_str(config_toml);

    assert!(
        result.is_err(),
        "Config with empty Fast tier should be rejected at startup, not fail at runtime"
    );

    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(
        err_msg.contains("fast") && err_msg.contains("no endpoints"),
        "Error should mention Fast tier has no endpoints, got: {}",
        err_msg
    );
}

#[test]
fn test_config_with_empty_deep_tier_rejected() {
    // P1 Regression: Rule router can route to Deep tier (High importance requests)
    // If Deep tier is empty, this fails at runtime instead of startup validation
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

# Deep tier is EMPTY - no endpoints
# Rule router routes High importance here, will fail at runtime
[models]
deep = []

[routing]
strategy = "rule"
router_tier = "balanced"
"#;

    let result = Config::from_str(config_toml);

    assert!(
        result.is_err(),
        "Config with empty Deep tier should be rejected at startup, not fail at runtime"
    );

    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(
        err_msg.contains("deep") && err_msg.contains("no endpoints"),
        "Error should mention Deep tier has no endpoints, got: {}",
        err_msg
    );
}

#[test]
fn test_config_with_empty_balanced_tier_rejected() {
    // Balanced tier is often used as fallback - must not be empty
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

# Balanced tier is EMPTY - no endpoints
# Commonly used tier, must have endpoints
[models]
balanced = []

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
router_tier = "fast"
"#;

    let result = Config::from_str(config_toml);

    assert!(
        result.is_err(),
        "Config with empty Balanced tier should be rejected"
    );

    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(
        err_msg.contains("balanced") && err_msg.contains("no endpoints"),
        "Error should mention Balanced tier has no endpoints, got: {}",
        err_msg
    );
}

#[test]
fn test_all_tiers_require_endpoints() {
    // Comprehensive test: ALL three tiers must have at least one endpoint
    // This prevents runtime failures when any tier is selected
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
temperature = 0.7
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 4096
temperature = 0.7
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 8192
temperature = 0.7
weight = 1.0
priority = 1

[routing]
strategy = "rule"
router_tier = "balanced"
"#;

    let result = Config::from_str(config_toml);

    assert!(
        result.is_ok(),
        "Config with all tiers populated should be accepted"
    );
}
