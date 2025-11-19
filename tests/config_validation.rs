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
router_model = "balanced"

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
router_model = "balanced"

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
router_model = "balanced"

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
router_model = "balanced"

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
router_model = "balanced"

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
router_model = "balanced"

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
router_model = "balanced"

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
