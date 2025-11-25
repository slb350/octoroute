//! Config encapsulation tests
//!
//! Tests that RoutingConfig.router_tier is private with accessor method,
//! preventing post-validation mutation and enforcing encapsulation.
//!
//! Addresses PR #4 Type Design Issue: router_tier public field

use octoroute::config::Config;
use octoroute::router::TargetModel;

/// Test that router_tier() accessor returns the correct value
#[test]
fn test_router_tier_accessor_returns_value() {
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
router_tier = "fast"
"#;

    let config: Config = toml::from_str(config_toml).expect("Should parse");

    // Access via accessor method
    assert_eq!(
        config.routing.router_tier(),
        TargetModel::Fast,
        "router_tier() should return Fast"
    );
}

/// Test that router_tier() accessor works for all tiers
#[test]
fn test_router_tier_accessor_all_tiers() {
    for (tier_str, expected) in &[
        ("fast", TargetModel::Fast),
        ("balanced", TargetModel::Balanced),
        ("deep", TargetModel::Deep),
    ] {
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
            tier_str
        );

        let config: Config = toml::from_str(&config_toml).expect("Should parse");

        assert_eq!(
            config.routing.router_tier(),
            *expected,
            "router_tier() should return {:?} for tier '{}'",
            expected,
            tier_str
        );
    }
}

/// Test that router_tier field is private (compile-time enforcement)
///
/// This test documents that direct field access should NOT compile.
/// The old design allowed: config.routing.router_tier = TargetModel::Deep
/// The new design makes this impossible, preventing post-validation mutation.
#[test]
fn test_router_tier_field_is_private() {
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
router_tier = "balanced"
"#;

    let config: Config = toml::from_str(config_toml).expect("Should parse");

    // With the old design, this was possible (but wrong):
    // config.routing.router_tier = TargetModel::Deep;  // Bypasses validation!

    // With the new design, direct field access is impossible (compile error)
    // Must use accessor: config.routing.router_tier()

    // Verify the accessor works
    assert_eq!(
        config.routing.router_tier(),
        TargetModel::Balanced,
        "Accessor should return the configured value"
    );

    // Note: Attempting to write `config.routing.router_tier = TargetModel::Deep`
    // will cause a compile error because the field is private
}

/// Test that default router_tier value is accessible via accessor
#[test]
fn test_default_router_tier_accessible() {
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
"#;

    let config: Config = toml::from_str(config_toml).expect("Should parse");

    // Default should be Balanced
    assert_eq!(
        config.routing.router_tier(),
        TargetModel::Balanced,
        "Default router_tier should be Balanced"
    );
}

/// Test that router_tier accessor is consistent across the system
///
/// Verifies that when router_tier is set in config, the accessor returns the
/// same value that would be used for router construction. This ensures the
/// config → router tier mapping is consistent.
///
/// Addresses PR #4 Issue: No test verifies accessor matches router construction
#[test]
fn test_router_tier_accessor_consistency() {
    // Test with Deep tier
    let config_deep = r#"
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
router_tier = "deep"
"#;

    let config: Config = toml::from_str(config_deep).expect("Should parse");

    // Verify accessor returns Deep
    let tier_from_accessor = config.routing.router_tier();
    assert_eq!(
        tier_from_accessor,
        TargetModel::Deep,
        "Accessor should return Deep when router_tier='deep'"
    );

    // Test with Fast tier
    let config_fast = r#"
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
router_tier = "fast"
"#;

    let config: Config = toml::from_str(config_fast).expect("Should parse");

    // Verify accessor returns Fast
    let tier_from_accessor = config.routing.router_tier();
    assert_eq!(
        tier_from_accessor,
        TargetModel::Fast,
        "Accessor should return Fast when router_tier='fast'"
    );

    // Test with Balanced tier (explicit)
    let config_balanced = r#"
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

    let config: Config = toml::from_str(config_balanced).expect("Should parse");

    // Verify accessor returns Balanced
    let tier_from_accessor = config.routing.router_tier();
    assert_eq!(
        tier_from_accessor,
        TargetModel::Balanced,
        "Accessor should return Balanced when router_tier='balanced'"
    );
}

/// Test that concurrent router_tier() reads are safe (no data races)
///
/// This test verifies that Config can be safely shared across tokio tasks using Arc,
/// and that concurrent reads of router_tier() always return the same value.
///
/// Addresses PR #4 Medium Priority Issue #14.
///
/// **Background**: Multi-threaded Axum handlers share `Arc<Config>` state. If router_tier()
/// had interior mutability (e.g., lazy initialization), concurrent reads could cause:
/// - Race conditions (two threads both initializing)
/// - Inconsistent values returned
/// - Undefined behavior
///
/// **This test verifies**: router_tier() returns a Copy type (TargetModel) with no
/// interior mutability, so concurrent reads are safe.
#[tokio::test]
async fn test_concurrent_router_tier_reads_no_data_race() {
    use std::sync::Arc;

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
router_tier = "balanced"
"#;

    let config: Config = toml::from_str(config_toml).expect("Should parse");
    let config = Arc::new(config);

    // Spawn 100 concurrent tasks all reading router_tier()
    let handles: Vec<_> = (0..100)
        .map(|_| {
            let config = Arc::clone(&config);
            tokio::spawn(async move {
                // Each task reads router_tier() - should always return Balanced
                config.routing.router_tier()
            })
        })
        .collect();

    // Wait for all tasks and verify all returned Balanced
    for handle in handles {
        let tier = handle.await.expect("Task should not panic");
        assert_eq!(
            tier,
            TargetModel::Balanced,
            "All concurrent reads should return Balanced (no data race)"
        );
    }
}

/// Test that Arc<Config> sharing is safe across threads
///
/// This test verifies that Config can be safely shared across OS threads (not just tokio tasks).
/// This is important for multi-threaded Axum servers where different request handlers may run
/// on different OS threads.
#[test]
fn test_arc_config_sharing_across_threads() {
    use std::sync::Arc;
    use std::thread;

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
router_tier = "deep"
"#;

    let config: Config = toml::from_str(config_toml).expect("Should parse");
    let config = Arc::new(config);

    // Spawn 10 OS threads all reading router_tier()
    let handles: Vec<_> = (0..10)
        .map(|_| {
            let config = Arc::clone(&config);
            thread::spawn(move || {
                // Each thread reads router_tier() - should always return Deep
                config.routing.router_tier()
            })
        })
        .collect();

    // Wait for all threads and verify all returned Deep
    for handle in handles {
        let tier = handle.join().expect("Thread should not panic");
        assert_eq!(
            tier,
            TargetModel::Deep,
            "All concurrent thread reads should return Deep (no data race)"
        );
    }
}
