//! Integration tests for configuration validation
//!
//! Verifies that invalid configurations are rejected at startup (Config::from_file())
//! rather than causing runtime errors. Tests the full path: file → parse → validate.

use octoroute::config::Config;
use std::io::Write;
use tempfile::NamedTempFile;

/// Helper to create a temporary config file with given TOML content
fn create_temp_config(toml_content: &str) -> NamedTempFile {
    let mut temp_file = NamedTempFile::new().expect("Failed to create temp file");
    temp_file
        .write_all(toml_content.as_bytes())
        .expect("Failed to write temp file");
    temp_file.flush().expect("Failed to flush temp file");
    temp_file
}

#[test]
fn test_config_from_file_rejects_base_url_without_v1() {
    let toml_content = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234"
max_tokens = 2048

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
router_tier = "balanced"

[observability]
log_level = "info"
"#;

    let temp_file = create_temp_config(toml_content);
    let result = Config::from_file(temp_file.path());

    assert!(
        result.is_err(),
        "Config::from_file should reject base_url without /v1"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("base_url") && err_msg.contains("/v1"),
        "Error message should mention base_url and /v1 requirement, got: {}",
        err_msg
    );
}

#[test]
fn test_config_from_file_rejects_zero_weight() {
    let toml_content = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
weight = 0.0

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
router_tier = "balanced"

[observability]
log_level = "info"
"#;

    let temp_file = create_temp_config(toml_content);
    let result = Config::from_file(temp_file.path());

    assert!(
        result.is_err(),
        "Config::from_file should reject zero weight"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("weight"),
        "Error message should mention weight, got: {}",
        err_msg
    );
}

#[test]
fn test_config_from_file_rejects_negative_weight() {
    let toml_content = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
weight = -1.5

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
router_tier = "balanced"

[observability]
log_level = "info"
"#;

    let temp_file = create_temp_config(toml_content);
    let result = Config::from_file(temp_file.path());

    assert!(
        result.is_err(),
        "Config::from_file should reject negative weight"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("weight"),
        "Error message should mention weight, got: {}",
        err_msg
    );
}

#[test]
fn test_config_from_file_rejects_zero_max_tokens() {
    let toml_content = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 0

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
router_tier = "balanced"

[observability]
log_level = "info"
"#;

    let temp_file = create_temp_config(toml_content);
    let result = Config::from_file(temp_file.path());

    assert!(
        result.is_err(),
        "Config::from_file should reject zero max_tokens"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("max_tokens"),
        "Error message should mention max_tokens, got: {}",
        err_msg
    );
}

#[test]
fn test_config_from_file_rejects_invalid_base_url_protocol() {
    let toml_content = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "test-fast"
base_url = "ftp://localhost:1234/v1"
max_tokens = 2048

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
router_tier = "balanced"

[observability]
log_level = "info"
"#;

    let temp_file = create_temp_config(toml_content);
    let result = Config::from_file(temp_file.path());

    assert!(
        result.is_err(),
        "Config::from_file should reject non-http/https base_url"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("base_url"),
        "Error message should mention base_url, got: {}",
        err_msg
    );
}

#[test]
fn test_config_from_file_accepts_valid_config() {
    let toml_content = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
weight = 1.0

[[models.balanced]]
name = "test-balanced"
base_url = "https://localhost:1235/v1"
max_tokens = 4096
weight = 2.0

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192
weight = 1.5

[routing]
strategy = "rule"
router_tier = "balanced"

[observability]
log_level = "info"
"#;

    let temp_file = create_temp_config(toml_content);
    let result = Config::from_file(temp_file.path());

    assert!(
        result.is_ok(),
        "Config::from_file should accept valid config, error: {:?}",
        result.err()
    );

    let config = result.unwrap();
    assert_eq!(config.models.fast.len(), 1);
    assert_eq!(config.models.balanced.len(), 1);
    assert_eq!(config.models.deep.len(), 1);
}

#[test]
fn test_config_from_file_rejects_missing_fast_tier() {
    let toml_content = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
router_tier = "balanced"

[observability]
log_level = "info"
"#;

    let temp_file = create_temp_config(toml_content);
    let result = Config::from_file(temp_file.path());

    assert!(
        result.is_err(),
        "Config::from_file should reject missing fast tier"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("fast"),
        "Error message should mention fast tier, got: {}",
        err_msg
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Per-Tier Timeout Validation Tests
// ═══════════════════════════════════════════════════════════════════════════
//
// These tests verify the TimeoutsConfig custom deserializer validation logic
// (src/config.rs:220-276) which enforces timeout bounds: (0, 300] seconds.
//
// Addresses PR review Issue #6 (CRITICAL): Missing test coverage for existing
// validation logic that prevents zero timeouts, excessive timeouts, and ensures
// boundary values work correctly.

#[test]
fn test_per_tier_timeout_zero_rejected() {
    // RED PHASE: Test that zero timeout is rejected during TOML deserialization
    //
    // Validates that TimeoutsConfig::new() enforces lower bound (timeout > 0)
    // to prevent infinite/nonsensical timeouts.
    let config_str = r#"
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
router_tier = "balanced"

[timeouts]
fast = 0  # Invalid: zero timeout
"#;

    let result: Result<Config, _> = toml::from_str(config_str);

    assert!(
        result.is_err(),
        "Config with zero per-tier timeout should be rejected at parse time"
    );

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("timeouts.fast") && err.contains("greater than 0"),
        "Error should mention timeouts.fast must be > 0, got: {}",
        err
    );
}

#[test]
fn test_per_tier_timeout_exceeds_300_rejected() {
    // RED PHASE: Test that timeout > 300 seconds is rejected during deserialization
    //
    // Validates that TimeoutsConfig::new() enforces upper bound (timeout <= 300)
    // to prevent config errors, unit confusion, and overflow risk.
    let config_str = r#"
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
router_tier = "balanced"

[timeouts]
deep = 301  # Invalid: exceeds 300 second limit
"#;

    let result: Result<Config, _> = toml::from_str(config_str);

    assert!(
        result.is_err(),
        "Config with per-tier timeout > 300 should be rejected at parse time"
    );

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("timeouts.deep") && err.contains("300"),
        "Error should mention timeouts.deep cannot exceed 300, got: {}",
        err
    );
}

#[test]
fn test_per_tier_timeout_boundary_values() {
    // GREEN PHASE: Test that boundary values (1, 300) are accepted
    //
    // Validates that minimum valid (1 second) and maximum valid (300 seconds)
    // timeouts are correctly accepted and accessible.
    let config_str = r#"
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
router_tier = "balanced"

[timeouts]
fast = 1     # Minimum valid value
balanced = 150  # Middle value
deep = 300   # Maximum valid value
"#;

    let config: Config = toml::from_str(config_str)
        .expect("Config with boundary timeout values (1, 300) should be valid");

    assert_eq!(
        config.timeouts.fast(),
        Some(1),
        "Fast tier should have 1 second timeout"
    );
    assert_eq!(
        config.timeouts.balanced(),
        Some(150),
        "Balanced tier should have 150 second timeout"
    );
    assert_eq!(
        config.timeouts.deep(),
        Some(300),
        "Deep tier should have 300 second timeout"
    );
}

/// Test that very small (but valid) weight values are accepted
///
/// Addresses PR #4 Medium Priority Issue #17: Config boundary tests
///
/// This test verifies that floating-point precision issues don't cause
/// rejection of very small but valid weight values.
#[test]
fn test_weight_boundary_very_small_valid() {
    let config_str = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 2048
weight = 0.0001  # Very small but valid (> 0)
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
router_tier = "balanced"
"#;

    let config: Config =
        toml::from_str(config_str).expect("Config with very small weight (0.0001) should be valid");

    config
        .validate()
        .expect("Config with weight=0.0001 should pass validation");

    // Verify weight was preserved correctly (no precision loss)
    assert_eq!(
        config.models.fast[0].weight(),
        0.0001,
        "Very small weight should be preserved exactly"
    );
}

/// Test that minimum max_tokens value is accepted
///
/// Addresses PR #4 Medium Priority Issue #17: Config boundary tests
///
/// While max_tokens=1 is technically valid, it's impractical for real use.
/// This test verifies the boundary condition is handled correctly.
#[test]
fn test_max_tokens_minimum_boundary() {
    let config_str = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-1"
base_url = "http://localhost:11434/v1"
max_tokens = 1  # Minimum valid value
weight = 1.0
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1234/v1"
max_tokens = 1  # Minimum valid value
weight = 1.0
priority = 1

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:8080/v1"
max_tokens = 1  # Minimum valid value
weight = 1.0
priority = 1

[routing]
strategy = "rule"
router_tier = "balanced"
"#;

    let config: Config = toml::from_str(config_str)
        .expect("Config with max_tokens=1 should be valid (boundary condition)");

    config
        .validate()
        .expect("Config with max_tokens=1 should pass validation");

    // Verify values
    assert_eq!(config.models.fast[0].max_tokens(), 1);
    assert_eq!(config.models.balanced[0].max_tokens(), 1);
    assert_eq!(config.models.deep[0].max_tokens(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// Endpoint Name Uniqueness Validation Tests
// ═══════════════════════════════════════════════════════════════════════════
//
// These tests verify that duplicate endpoint names across tiers are rejected.
// This prevents silent failures where find_endpoint_by_name returns the first
// match (fast tier), making endpoints in lower tiers inaccessible by name.

#[test]
fn test_config_rejects_duplicate_endpoint_names_across_tiers() {
    // Test that duplicate endpoint names ACROSS different tiers are rejected
    //
    // When the same endpoint name exists in multiple tiers, find_endpoint_by_name
    // will only ever return the first match (fast tier), making the others
    // inaccessible. This is a silent configuration error that should be caught.
    let toml_content = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "shared-model"
base_url = "http://localhost:1234/v1"
max_tokens = 2048

[[models.balanced]]
name = "shared-model"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
router_tier = "balanced"

[observability]
log_level = "info"
"#;

    let temp_file = create_temp_config(toml_content);
    let result = Config::from_file(temp_file.path());

    assert!(
        result.is_err(),
        "Config::from_file should reject duplicate endpoint names across tiers"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("shared-model")
            && err_msg.contains("fast")
            && err_msg.contains("balanced"),
        "Error message should mention the duplicate endpoint name and both tiers, got: {}",
        err_msg
    );
}

#[test]
fn test_config_accepts_duplicate_endpoint_names_within_same_tier() {
    // Duplicates WITHIN the same tier are allowed for load balancing
    // (e.g., same model served from multiple machines)
    let toml_content = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-model"
base_url = "http://localhost:1234/v1"
max_tokens = 2048

[[models.fast]]
name = "fast-model"
base_url = "http://localhost:1237/v1"
max_tokens = 2048

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
router_tier = "balanced"

[observability]
log_level = "info"
"#;

    let temp_file = create_temp_config(toml_content);
    let result = Config::from_file(temp_file.path());

    assert!(
        result.is_ok(),
        "Config::from_file should accept duplicate endpoint names within same tier (load balancing), error: {:?}",
        result.err()
    );

    let config = result.unwrap();
    // Both endpoints with same name should be loaded
    assert_eq!(config.models.fast.len(), 2);
}

#[test]
fn test_config_accepts_unique_endpoint_names() {
    // Verify that unique endpoint names across all tiers are accepted
    let toml_content = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-model-1"
base_url = "http://localhost:1234/v1"
max_tokens = 2048

[[models.fast]]
name = "fast-model-2"
base_url = "http://localhost:1237/v1"
max_tokens = 2048

[[models.balanced]]
name = "balanced-model"
base_url = "http://localhost:1235/v1"
max_tokens = 4096

[[models.deep]]
name = "deep-model"
base_url = "http://localhost:1236/v1"
max_tokens = 8192

[routing]
strategy = "rule"
router_tier = "balanced"

[observability]
log_level = "info"
"#;

    let temp_file = create_temp_config(toml_content);
    let result = Config::from_file(temp_file.path());

    assert!(
        result.is_ok(),
        "Config::from_file should accept unique endpoint names, error: {:?}",
        result.err()
    );

    let config = result.unwrap();
    assert_eq!(config.models.fast.len(), 2);
    assert_eq!(config.models.balanced.len(), 1);
    assert_eq!(config.models.deep.len(), 1);
}
