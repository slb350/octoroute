/// Tests for priority-based fallback when primary endpoints are unhealthy
///
/// Verifies that ModelSelector correctly falls back to lower-priority endpoints
/// when all higher-priority endpoints are unhealthy.
///
/// RATIONALE: Bug could cause complete tier exhaustion when only primary endpoints
/// fail, even though fallback endpoints are available and healthy.
use octoroute::config::Config;
use octoroute::metrics::Metrics;
use octoroute::models::EndpointName;
use octoroute::models::selector::ModelSelector;
use octoroute::router::TargetModel;
use std::collections::HashSet;
use std::sync::Arc;

/// Test that selector falls back to priority=1 when priority=2 is unhealthy
///
/// SCENARIO: Tier has two endpoints:
/// - endpoint-1 (priority=2, primary) - marked unhealthy after 3 failures
/// - endpoint-2 (priority=1, fallback) - healthy
///
/// EXPECTED: Selector should return endpoint-2 (fallback), NOT None.
/// Users should never see "no healthy endpoints" when fallbacks are available.
///
/// RATIONALE: Per docs/configuration.md, higher priority values = tried first.
/// When priority=2 fails, system should fall back to priority=1.
#[tokio::test]
async fn test_priority_fallback_when_primary_unhealthy() {
    // ARRANGE: Create config with primary (priority=2) + fallback (priority=1) endpoints
    let toml = r#"
        [server]
        host = "127.0.0.1"
        port = 3000

        [[models.fast]]
        name = "fast-primary"
        base_url = "http://localhost:11434/v1"
        max_tokens = 4096
        priority = 2
        weight = 1.0

        [[models.fast]]
        name = "fast-fallback"
        base_url = "http://localhost:11435/v1"
        max_tokens = 4096
        priority = 1
        weight = 1.0

        [[models.balanced]]
        name = "balanced"
        base_url = "http://localhost:1234/v1"
        max_tokens = 8192

        [[models.deep]]
        name = "deep"
        base_url = "http://localhost:8080/v1"
        max_tokens = 16384

        [routing]
        strategy = "rule"
    "#;

    let config: Config = toml::from_str(toml).expect("Failed to parse config");
    config.validate().expect("Config validation failed");

    let metrics = Arc::new(Metrics::new().expect("Failed to create metrics"));
    let selector = Arc::new(ModelSelector::new(Arc::new(config), metrics));

    // Mark primary endpoint as unhealthy (3 consecutive failures)
    for _ in 0..3 {
        selector
            .health_checker()
            .mark_failure("fast-primary")
            .await
            .expect("Failed to mark failure");
    }

    // Verify primary is unhealthy
    let primary_healthy = selector.health_checker().is_healthy("fast-primary").await;
    assert!(
        !primary_healthy,
        "Primary endpoint should be unhealthy after 3 failures"
    );

    // Verify fallback is still healthy
    let fallback_healthy = selector.health_checker().is_healthy("fast-fallback").await;
    assert!(fallback_healthy, "Fallback endpoint should be healthy");

    // ACT: Select from fast tier with empty exclusion set
    let exclusions: HashSet<EndpointName> = HashSet::new();
    let selected = selector.select(TargetModel::Fast, &exclusions).await;

    // ASSERT: Should return fallback endpoint, NOT None
    assert!(
        selected.is_some(),
        "Selector should return fallback endpoint when primary is unhealthy"
    );

    let endpoint = selected.unwrap();
    assert_eq!(
        endpoint.name(),
        "fast-fallback",
        "Should select fallback endpoint (priority=1) when primary (priority=2) is unhealthy"
    );
    assert_eq!(
        endpoint.priority(),
        1,
        "Fallback should have priority=1 (lower than primary's priority=2)"
    );
}

/// Test that selector returns None only when ALL priorities are exhausted
///
/// SCENARIO: Both primary (priority=2) and fallback (priority=1) are unhealthy
///
/// EXPECTED: Selector returns None (no healthy endpoints available)
#[tokio::test]
async fn test_returns_none_when_all_priorities_exhausted() {
    // ARRANGE: Create config with primary (priority=2) + fallback (priority=1) endpoints
    let toml = r#"
        [server]
        host = "127.0.0.1"
        port = 3000

        [[models.fast]]
        name = "fast-primary"
        base_url = "http://localhost:11434/v1"
        max_tokens = 4096
        priority = 2

        [[models.fast]]
        name = "fast-fallback"
        base_url = "http://localhost:11435/v1"
        max_tokens = 4096
        priority = 1

        [[models.balanced]]
        name = "balanced"
        base_url = "http://localhost:1234/v1"
        max_tokens = 8192

        [[models.deep]]
        name = "deep"
        base_url = "http://localhost:8080/v1"
        max_tokens = 16384

        [routing]
        strategy = "rule"
    "#;

    let config: Config = toml::from_str(toml).expect("Failed to parse config");
    config.validate().expect("Config validation failed");

    let metrics = Arc::new(Metrics::new().expect("Failed to create metrics"));
    let selector = Arc::new(ModelSelector::new(Arc::new(config), metrics));

    // Mark BOTH endpoints as unhealthy
    for _ in 0..3 {
        selector
            .health_checker()
            .mark_failure("fast-primary")
            .await
            .expect("Failed to mark primary failure");
        selector
            .health_checker()
            .mark_failure("fast-fallback")
            .await
            .expect("Failed to mark fallback failure");
    }

    // ACT: Select from fast tier
    let exclusions: HashSet<EndpointName> = HashSet::new();
    let selected = selector.select(TargetModel::Fast, &exclusions).await;

    // ASSERT: Should return None (all priorities exhausted)
    assert!(
        selected.is_none(),
        "Selector should return None when all priorities are exhausted"
    );
}

/// Test that priorities are respected during selection
///
/// SCENARIO: Both priority=2 and priority=1 are healthy
///
/// EXPECTED: Selector should ONLY choose from priority=2 (never priority=1)
/// when higher priority is available.
///
/// RATIONALE: Per docs/configuration.md, higher priority values = tried first.
/// Priority=2 endpoints should always be selected over priority=1 when healthy.
#[tokio::test]
async fn test_respects_priority_order_when_all_healthy() {
    // ARRANGE: Create config with primary (priority=2) + fallback (priority=1) endpoints
    let toml = r#"
        [server]
        host = "127.0.0.1"
        port = 3000

        [[models.fast]]
        name = "fast-primary"
        base_url = "http://localhost:11434/v1"
        max_tokens = 4096
        priority = 2

        [[models.fast]]
        name = "fast-fallback"
        base_url = "http://localhost:11435/v1"
        max_tokens = 4096
        priority = 1

        [[models.balanced]]
        name = "balanced"
        base_url = "http://localhost:1234/v1"
        max_tokens = 8192

        [[models.deep]]
        name = "deep"
        base_url = "http://localhost:8080/v1"
        max_tokens = 16384

        [routing]
        strategy = "rule"
    "#;

    let config: Config = toml::from_str(toml).expect("Failed to parse config");
    config.validate().expect("Config validation failed");

    let metrics = Arc::new(Metrics::new().expect("Failed to create metrics"));
    let selector = Arc::new(ModelSelector::new(Arc::new(config), metrics));

    // ACT: Select 100 times and verify we ALWAYS get primary (priority=2), NEVER fallback (priority=1)
    let exclusions: HashSet<EndpointName> = HashSet::new();
    for _ in 0..100 {
        let selected = selector.select(TargetModel::Fast, &exclusions).await;
        assert!(selected.is_some(), "Should always select an endpoint");

        let endpoint = selected.unwrap();
        assert_eq!(
            endpoint.name(),
            "fast-primary",
            "Should only select priority=2 endpoint (primary) when healthy, NEVER priority=1 (fallback)"
        );
        assert_eq!(
            endpoint.priority(),
            2,
            "Should select highest priority value (2 > 1)"
        );
    }
}
