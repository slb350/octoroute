//! Test for priority tier exhaustion scenarios
//!
//! Verifies that when all endpoints in the highest priority tier are unhealthy,
//! the system correctly falls back to lower priority tiers.

use octoroute::config::Config;
use octoroute::models::{ExclusionSet, ModelSelector};
use octoroute::router::TargetModel;
use std::sync::Arc;

fn create_priority_test_config() -> Config {
    let toml = r#"
[server]
host = "127.0.0.1"
port = 3000
request_timeout_seconds = 30

[[models.fast]]
name = "fast-priority-3-a"
base_url = "http://localhost:1234/v1"
max_tokens = 2048
priority = 3

[[models.fast]]
name = "fast-priority-3-b"
base_url = "http://localhost:1235/v1"
max_tokens = 2048
priority = 3

[[models.fast]]
name = "fast-priority-1"
base_url = "http://localhost:1236/v1"
max_tokens = 2048
priority = 1

[[models.balanced]]
name = "balanced-1"
base_url = "http://localhost:1237/v1"
max_tokens = 4096

[[models.deep]]
name = "deep-1"
base_url = "http://localhost:1238/v1"
max_tokens = 8192

[routing]
strategy = "rule"
router_model = "balanced"
"#;
    toml::from_str(toml).expect("should parse TOML")
}

#[tokio::test]
async fn test_priority_tier_exhaustion_falls_back_to_lower_priority() {
    // This test verifies that when all endpoints in the highest priority tier
    // are marked unhealthy, the selector correctly falls back to lower priority tiers.
    //
    // Setup:
    // - 2 endpoints at priority 3 (highest)
    // - 1 endpoint at priority 1 (lower)
    //
    // Steps:
    // 1. Mark both priority-3 endpoints as unhealthy
    // 2. Verify selector returns the priority-1 endpoint
    // 3. Verify service continues (doesn't fail when preferred tier unavailable)

    let config = Arc::new(create_priority_test_config());
    let selector = ModelSelector::new(config.clone());

    // Initially, should select from priority 3 tier
    let no_exclude = ExclusionSet::new();
    let initial_endpoint = selector
        .select(TargetModel::Fast, &no_exclude)
        .await
        .expect("should select endpoint");

    assert_eq!(
        initial_endpoint.priority(),
        3,
        "Initially should select from highest priority tier"
    );

    // Mark both priority-3 endpoints as unhealthy
    selector
        .health_checker()
        .mark_failure("fast-priority-3-a")
        .await
        .unwrap();
    selector
        .health_checker()
        .mark_failure("fast-priority-3-a")
        .await
        .unwrap();
    selector
        .health_checker()
        .mark_failure("fast-priority-3-a")
        .await
        .unwrap(); // 3rd failure makes it unhealthy

    selector
        .health_checker()
        .mark_failure("fast-priority-3-b")
        .await
        .unwrap();
    selector
        .health_checker()
        .mark_failure("fast-priority-3-b")
        .await
        .unwrap();
    selector
        .health_checker()
        .mark_failure("fast-priority-3-b")
        .await
        .unwrap(); // 3rd failure makes it unhealthy

    // Verify both priority-3 endpoints are now unhealthy
    assert!(
        !selector
            .health_checker()
            .is_healthy("fast-priority-3-a")
            .await
    );
    assert!(
        !selector
            .health_checker()
            .is_healthy("fast-priority-3-b")
            .await
    );

    // Now selection should fall back to priority-1 endpoint
    for _ in 0..10 {
        let endpoint = selector
            .select(TargetModel::Fast, &no_exclude)
            .await
            .expect("should still select endpoint even when preferred tier is down");

        assert_eq!(
            endpoint.name(),
            "fast-priority-1",
            "Should fall back to lower priority tier when highest tier is exhausted"
        );
        assert_eq!(endpoint.priority(), 1);
    }

    println!("✓ Priority tier exhaustion correctly falls back to lower priority tiers");
}

#[tokio::test]
async fn test_all_priorities_unhealthy_returns_none() {
    // Verify that when ALL endpoints (all priorities) are unhealthy, selector returns None

    let config = Arc::new(create_priority_test_config());
    let selector = ModelSelector::new(config.clone());

    // Mark ALL fast endpoints unhealthy
    for endpoint_name in ["fast-priority-3-a", "fast-priority-3-b", "fast-priority-1"] {
        for _ in 0..3 {
            selector
                .health_checker()
                .mark_failure(endpoint_name)
                .await
                .unwrap();
        }
        assert!(!selector.health_checker().is_healthy(endpoint_name).await);
    }

    // Now selection should return None
    let no_exclude = ExclusionSet::new();
    let result = selector.select(TargetModel::Fast, &no_exclude).await;

    assert!(
        result.is_none(),
        "Should return None when all endpoints in all priority tiers are unhealthy"
    );

    println!("✓ Returns None when all priority tiers are exhausted");
}
