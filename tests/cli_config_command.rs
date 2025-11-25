//! Integration tests for CLI config command
//!
//! Tests file I/O operations for the `octoroute config` subcommand.
//! Verifies template generation, file writing, and error handling.

use octoroute::cli::generate_config_template;
use octoroute::config::Config;
use std::fs;
use tempfile::TempDir;

/// Helper to create temporary directory for file operations
fn create_temp_dir() -> TempDir {
    TempDir::new().expect("Failed to create temp directory")
}

// ─────────────────────────────────────────────────────────────────────────────
// Template Content Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_generated_template_creates_valid_config_file() {
    let temp_dir = create_temp_dir();
    let config_path = temp_dir.path().join("config.toml");

    // Write template to file
    let template = generate_config_template();
    fs::write(&config_path, template).expect("Failed to write template");

    // Verify file can be loaded as valid Config
    let config =
        Config::from_file(&config_path).expect("Generated template should load as valid Config");

    // Verify structure
    assert!(!config.models.fast.is_empty());
    assert!(!config.models.balanced.is_empty());
    assert!(!config.models.deep.is_empty());
    assert_eq!(
        config.routing.strategy,
        octoroute::config::RoutingStrategy::Hybrid
    );
}

#[test]
fn test_template_file_content_matches_generation() {
    let temp_dir = create_temp_dir();
    let config_path = temp_dir.path().join("config.toml");

    let template = generate_config_template();
    fs::write(&config_path, template).expect("Failed to write template");

    let content = fs::read_to_string(&config_path).expect("Failed to read back");
    assert_eq!(content, template);
}

#[test]
fn test_template_has_all_required_sections() {
    let template = generate_config_template();

    assert!(template.contains("[server]"), "Missing [server]");
    assert!(
        template.contains("[[models.fast]]"),
        "Missing [[models.fast]]"
    );
    assert!(
        template.contains("[[models.balanced]]"),
        "Missing [[models.balanced]]"
    );
    assert!(
        template.contains("[[models.deep]]"),
        "Missing [[models.deep]]"
    );
    assert!(template.contains("[routing]"), "Missing [routing]");
    assert!(
        template.contains("[observability]"),
        "Missing [observability]"
    );
    assert!(template.contains("[timeouts]"), "Missing [timeouts]");
}

#[test]
fn test_template_includes_documentation() {
    let template = generate_config_template();

    // Check for documentation comments
    assert!(template.contains("# "), "Template should have comments");
    assert!(
        template.contains("Octoroute"),
        "Template should have header"
    );
    assert!(
        template.contains("MODEL TIERS") || template.contains("model tier"),
        "Template should document tiers"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// File Operation Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_file_exists_detection() {
    let temp_dir = create_temp_dir();
    let config_path = temp_dir.path().join("config.toml");

    // File doesn't exist yet
    assert!(!config_path.exists());

    // Create file
    fs::write(&config_path, "existing content").expect("Failed to create file");

    // File now exists
    assert!(config_path.exists());
}

#[test]
fn test_write_to_nonexistent_parent_fails() {
    let temp_dir = create_temp_dir();
    let bad_path = temp_dir.path().join("nonexistent").join("config.toml");

    let result = fs::write(&bad_path, "test");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn test_template_roundtrip_preserves_config() {
    let temp_dir = create_temp_dir();
    let config_path = temp_dir.path().join("config.toml");

    // Write template
    let template = generate_config_template();
    fs::write(&config_path, template).expect("Failed to write template");

    // Load config
    let config = Config::from_file(&config_path).expect("Failed to load config");

    // Verify key settings are correct
    assert_eq!(config.server.host, "0.0.0.0");
    assert_eq!(config.server.port, 3000);
    assert_eq!(config.server.request_timeout_seconds, 30);
    assert_eq!(config.observability.log_level, "info");
}
