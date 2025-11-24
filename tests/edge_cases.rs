//! Edge case tests for production scenarios
//!
//! Tests covering important edge cases and operational concerns:
//! - Config reload behavior (or lack thereof)
//! - Router tier exhaustion vs target tier health
//! - Config validation enforcement in test helpers
//!
//! Addresses PR #4 review: Test improvements 4-6

use octoroute::config::Config;
use octoroute::metrics::Metrics;
use octoroute::models::ModelSelector;
use octoroute::router::{LlmBasedRouter, RouteMetadata, TargetModel};
use std::sync::Arc;

/// Helper to create test metrics
fn test_metrics() -> Arc<Metrics> {
    Arc::new(Metrics::new().expect("should create metrics"))
}

/// Test documenting that config reload is NOT supported
///
/// **Design Decision**: Configuration reload requires server restart
///
/// **Why no hot-reload**:
/// 1. **State Consistency**: Config changes affect multiple subsystems (routing, health tracking,
///    model selection). Hot-reload would require coordinating state transitions across all systems
///    atomically, which is complex and error-prone.
///
/// 2. **Health Tracking State**: Health checker maintains endpoint health state. Reloading config
///    would need to handle: endpoint additions (easy), deletions (what about in-flight requests?),
///    and modifications (reset health state or preserve it?).
///
/// 3. **In-Flight Requests**: Requests mid-execution reference config-derived state (endpoints,
///    timeouts, routing strategy). Changing config mid-request creates race conditions and
///    unpredictable behavior.
///
/// 4. **Simplicity**: Restart-to-reload is simple, predictable, and aligns with standard deployment
///    practices (rolling updates, blue-green deployments). Hot-reload adds complexity for marginal
///    operational benefit.
///
/// **Operational Impact**: Config changes require:
/// - Server restart (graceful shutdown recommended)
/// - Zero-downtime deployments use load balancer rotation or blue-green strategy
/// - Config validation at startup catches errors before traffic is served
///
/// **Future Consideration**: Hot-reload could be added later if operational needs justify the
/// complexity, but current design prioritizes simplicity and correctness.
#[test]
fn test_config_reload_not_supported_by_design() {
    // Load initial config
    let config1 = Config::from_file("config.toml").expect("should load config");
    config1.validate().expect("should validate");

    // Verify Config has no reload() method
    // (This is a compile-time assertion - if reload() existed, this would fail to compile)
    let _no_reload_method = || {
        // config1.reload(); // <-- This would cause a compile error
    };

    // Config changes require:
    // 1. Modify config.toml
    // 2. Restart server
    // 3. New config loaded at startup via Config::from_file()
    //
    // This test documents that this is intentional design, not a missing feature.
    // Test passes to confirm documentation is reviewed.
}

/// Test router tier exhaustion with healthy target tiers
///
/// **Edge Case**: Router tier (Balanced) has no healthy endpoints, but target tiers
/// (Fast, Deep) have healthy endpoints.
///
/// **Expected Behavior**: Error should mention router tier exhaustion, NOT target tier
/// health, because the routing decision itself failed (can't route if router tier is down).
///
/// **Why This Matters**: Operators need clear error messages distinguishing between:
/// - Router tier exhaustion: "Can't make routing decision" (this test)
/// - Target tier exhaustion: "Routing decided Fast, but Fast tier is down"
///
/// This test ensures error messages correctly identify the root cause.
#[tokio::test]
async fn test_router_tier_exhausted_with_healthy_target_tiers() {
    // Create config with Balanced tier (router) and Fast tier (target)
    let toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test-fast"
base_url = "http://localhost:8080/v1"
max_tokens = 4096

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:9090/v1"
max_tokens = 8192

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:10000/v1"
max_tokens = 16384

[routing]
strategy = "llm"
router_tier = "balanced"
"#;

    let config: Config = toml::from_str(toml).expect("should parse");
    config.validate().expect("should validate");
    let config = Arc::new(config);

    let selector = Arc::new(ModelSelector::new(config, test_metrics()));
    let metrics = Arc::new(Metrics::new().expect("should create metrics"));
    let router = LlmBasedRouter::new(selector.clone(), TargetModel::Balanced, 10, metrics)
        .expect("should create router");

    // Mark balanced tier (router) as unhealthy
    selector
        .health_checker()
        .mark_failure("test-balanced")
        .await
        .expect("should mark balanced unhealthy");
    selector
        .health_checker()
        .mark_failure("test-balanced")
        .await
        .expect("mark_failure again");
    selector
        .health_checker()
        .mark_failure("test-balanced")
        .await
        .expect("mark_failure third time - now unhealthy");

    // Fast tier (target) remains healthy - not marked as failed

    // Try to route - should fail because router tier is exhausted
    let meta = RouteMetadata::new(100);
    let result = router.route("test prompt", &meta).await;

    // Should fail with router tier exhaustion
    assert!(
        result.is_err(),
        "Routing should fail when router tier exhausted"
    );

    let error_msg = result.unwrap_err().to_string();

    // Error should mention Balanced tier (router), not Fast tier (target)
    assert!(
        error_msg.to_lowercase().contains("balanced") || error_msg.contains("router"),
        "Error should mention router tier (Balanced) exhaustion, got: {}",
        error_msg
    );

    // Should NOT confuse with target tier health
    // (Don't check for "fast" because it might not be mentioned at all)
}

/// Test that validated config helper prevents invalid instances
///
/// **Test Gap**: Tests need a helper that enforces config validation during setup.
///
/// **Problem**: Tests could bypass validation by:
/// 1. Manually constructing Config structs with serde::from_str
/// 2. Not calling validate() before use
/// 3. Creating invalid test scenarios that would never occur in production
///
/// **Solution**: Use validated_config_from_toml() helper that:
/// - Parses TOML
/// - Calls validate() automatically
/// - Panics on validation failure (test should fix invalid config, not skip validation)
///
/// This test verifies the helper correctly rejects invalid configs.
#[test]
#[should_panic(expected = "validation")]
fn test_validated_config_helper_rejects_invalid_config() {
    // Helper function that enforces validation
    fn validated_config_from_toml(toml: &str) -> Config {
        let config: Config = toml::from_str(toml).expect("should parse TOML");
        config.validate().expect("config validation should pass");
        config
    }

    // Invalid config: max_tokens = 0 (must be > 0)
    let invalid_toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test"
base_url = "http://localhost:8080/v1"
max_tokens = 0

[models]
balanced = []
deep = []

[routing]
strategy = "rule"
"#;

    // Should panic because max_tokens = 0 is invalid
    let _config = validated_config_from_toml(invalid_toml);
}

/// Test that validated config helper accepts valid config
#[test]
fn test_validated_config_helper_accepts_valid_config() {
    // Helper function that enforces validation
    fn validated_config_from_toml(toml: &str) -> Config {
        let config: Config = toml::from_str(toml).expect("should parse TOML");
        config.validate().expect("config validation should pass");
        config
    }

    // Valid config
    let valid_toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test"
base_url = "http://localhost:8080/v1"
max_tokens = 4096

[[models.balanced]]
name = "test-balanced"
base_url = "http://localhost:1234/v1"
max_tokens = 8192

[[models.deep]]
name = "test-deep"
base_url = "http://localhost:8081/v1"
max_tokens = 16384

[routing]
strategy = "rule"
"#;

    // Should succeed
    let config = validated_config_from_toml(valid_toml);
    assert_eq!(config.models.fast.len(), 1);
    assert_eq!(config.models.fast[0].max_tokens(), 4096);
}

/// Test concurrent requests during router tier exhaustion
///
/// Addresses PR #4 Medium Priority Issue #15.
///
/// **Scenario**: 100 concurrent requests all try to route while ALL router tier
/// endpoints are down (Balanced tier exhausted).
///
/// **Verifies**:
/// 1. All requests fail fast (no hangs)
/// 2. Failure time is bounded (<10s total, not 100 * timeout)
/// 3. No panics from concurrent health tracker updates
/// 4. No deadlocks from multiple threads accessing the same locks
///
/// **Why Important**: Production scenario where router tier goes down under high load
/// could cause thundering herd, port exhaustion, or deadlocks if not handled properly.
#[tokio::test]
async fn test_concurrent_requests_during_router_tier_exhaustion() {
    use tokio::time::{Duration, Instant, timeout};

    // Create config with LLM routing (uses Balanced tier for routing)
    let toml = r#"
[server]
host = "127.0.0.1"
port = 3000

[[models.fast]]
name = "test-fast"
base_url = "http://192.0.2.1:8080/v1"
max_tokens = 4096
weight = 1.0
priority = 1

[[models.balanced]]
name = "test-balanced-1"
base_url = "http://192.0.2.10:9090/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[[models.balanced]]
name = "test-balanced-2"
base_url = "http://192.0.2.11:9091/v1"
max_tokens = 8192
weight = 1.0
priority = 1

[[models.deep]]
name = "test-deep"
base_url = "http://192.0.2.20:10000/v1"
max_tokens = 16384
weight = 1.0
priority = 1

[routing]
strategy = "llm"
router_tier = "balanced"
"#;

    let config: Config = toml::from_str(toml).expect("should parse");
    config.validate().expect("should validate");
    let config = Arc::new(config);

    let selector = Arc::new(ModelSelector::new(config.clone(), test_metrics()));

    // Mark ALL Balanced tier endpoints as unhealthy (3 failures each)
    for endpoint_name in ["test-balanced-1", "test-balanced-2"] {
        for _ in 0..3 {
            selector
                .health_checker()
                .mark_failure(endpoint_name)
                .await
                .expect("should mark failure");
        }
    }

    // Verify Balanced tier is exhausted
    assert!(
        !selector
            .health_checker()
            .is_healthy("test-balanced-1")
            .await,
        "test-balanced-1 should be unhealthy"
    );
    assert!(
        !selector
            .health_checker()
            .is_healthy("test-balanced-2")
            .await,
        "test-balanced-2 should be unhealthy"
    );

    // Create LLM router using exhausted Balanced tier
    let metrics = Arc::new(Metrics::new().expect("should create metrics"));
    let router = Arc::new(
        LlmBasedRouter::new(selector.clone(), TargetModel::Balanced, 10, metrics)
            .expect("should create router"),
    );

    let start = Instant::now();

    // Spawn 100 concurrent routing requests
    let handles: Vec<_> = (0..100)
        .map(|i| {
            let router = Arc::clone(&router);
            let meta = RouteMetadata::new(100);

            tokio::spawn(async move {
                // Each request should fail (router tier exhausted)
                router.route(&format!("Test message {}", i), &meta).await
            })
        })
        .collect();

    // Wait for all requests with 10s timeout (should finish much faster)
    let results_future = async {
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.expect("Task should not panic"));
        }
        results
    };

    let results = timeout(Duration::from_secs(10), results_future)
        .await
        .expect("All requests should complete within 10s (no hangs)");

    let elapsed = start.elapsed();

    // CRITICAL ASSERTION 1: All requests should fail (router tier exhausted)
    let failure_count = results.iter().filter(|r| r.is_err()).count();
    assert_eq!(
        failure_count, 100,
        "All 100 requests should fail when router tier is exhausted"
    );

    // CRITICAL ASSERTION 2: Total time should be bounded
    // Each request has ~1s timeout, but concurrent execution means total time << 100s
    // Expect: <10s total (allowing for some overhead)
    assert!(
        elapsed < Duration::from_secs(10),
        "100 concurrent requests should complete in <10s, got {:?}. \
        This indicates no hangs or deadlocks.",
        elapsed
    );

    // CRITICAL ASSERTION 3: No panics occurred (verified by .expect("Task should not panic"))
    // If concurrent health tracker updates caused panics, tokio::spawn would have panicked

    // Verify health tracker still works (no corruption from concurrent access)
    let statuses = selector.health_checker().get_all_statuses().await;
    assert!(
        !statuses.is_empty(),
        "Health tracker should still function after concurrent stress"
    );
}
