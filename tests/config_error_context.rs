//! Tests for config error context preservation
//!
//! Verifies that configuration parsing errors preserve the original error
//! context from std::io::Error and toml::de::Error for better debugging.

use octoroute::config::Config;
use std::error::Error;
use std::str::FromStr;

#[test]
fn test_config_file_read_error_preserves_io_error() {
    // Attempt to read a file that doesn't exist
    let result = Config::from_file("/nonexistent/path/to/config.toml");

    assert!(result.is_err(), "Reading nonexistent file should fail");

    let err = result.unwrap_err();

    // Verify we can access the source io::Error through the error chain
    assert!(
        err.source().is_some(),
        "Error should have a source (io::Error)"
    );

    // Verify the error message includes the path and io error details
    let err_string = err.to_string();
    assert!(
        err_string.contains("/nonexistent/path/to/config.toml"),
        "Error should include the file path, got: {}",
        err_string
    );

    // Verify we can find the io::Error in the error chain
    let source = err.source().expect("Should have source error");
    assert!(
        source.is::<std::io::Error>(),
        "Source error should be io::Error, got: {:?}",
        source
    );
}

#[test]
fn test_config_parse_error_preserves_toml_error() {
    // Create invalid TOML that will fail parsing
    let invalid_toml = r#"
this is [[[[ not valid toml
it has {{{{ broken syntax
"#;

    let result = Config::from_str(invalid_toml);

    assert!(result.is_err(), "Parsing invalid TOML should fail");

    let err = result.unwrap_err();

    // Verify we can access the source toml::de::Error through the error chain
    assert!(
        err.source().is_some(),
        "Error should have a source (toml::de::Error)"
    );

    // Verify the error message includes parse error details
    let err_string = err.to_string();
    assert!(
        err_string.contains("parse") || err_string.contains("TOML"),
        "Error should indicate TOML parsing failure, got: {}",
        err_string
    );
}

#[test]
fn test_error_chain_is_preserved_through_conversion() {
    // Test that the full error chain is accessible for debugging
    let result = Config::from_file("/nonexistent/config.toml");

    assert!(result.is_err());
    let err = result.unwrap_err();

    // Walk the error chain to verify it's preserved
    let mut source = err.source();
    let mut found_io_error = false;
    let mut chain_length = 0;

    while let Some(s) = source {
        chain_length += 1;
        if s.is::<std::io::Error>() {
            found_io_error = true;

            // Verify we can downcast to the original error type
            let io_err = s
                .downcast_ref::<std::io::Error>()
                .expect("Should be able to downcast to io::Error");

            // Verify the io::Error has useful information
            assert!(
                matches!(io_err.kind(), std::io::ErrorKind::NotFound),
                "Should be NotFound error, got: {:?}",
                io_err.kind()
            );
            break;
        }
        source = s.source();
    }

    assert!(
        found_io_error,
        "Should find io::Error in error chain (chain length: {})",
        chain_length
    );
    assert!(
        chain_length > 0,
        "Error chain should have at least one source error"
    );
}

#[test]
fn test_config_validation_error_provides_context() {
    // Test that validation errors include helpful context when no endpoints are defined
    let config_toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[routing]
strategy = "llm"
router_tier = "balanced"

# No endpoints defined - should fail validation
"#;

    let result = Config::from_str(config_toml);

    // Should fail during TOML parsing (missing required sections)
    assert!(
        result.is_err(),
        "Config should fail when no model endpoints are defined"
    );

    let err = result.unwrap_err();
    let err_msg = format!("{}", err);

    // Error should mention missing model configuration
    // (toml crate will report missing required table)
    assert!(
        err_msg.to_lowercase().contains("missing") || err_msg.to_lowercase().contains("models"),
        "Error should indicate missing models configuration, got: {}",
        err_msg
    );
}

#[test]
fn test_permission_denied_error_preserves_context() {
    // Test that permission errors are properly preserved
    // (This test may not work in all environments, but demonstrates the pattern)

    // Skip this test if we can't create a file to set permissions on
    // In real implementation, we'd use a proper test fixture

    // The test would verify that when reading a file with permission denied,
    // the io::Error (PermissionDenied) is preserved in the error chain
}
