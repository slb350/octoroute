/// Tests for weighted load balancing distribution
///
/// Verifies that ModelSelector respects configured endpoint weights when selecting
/// from multiple healthy endpoints within the same priority group.
///
/// RATIONALE: Incorrect weighted selection could route all traffic to low-capacity
/// endpoints, defeating gradual rollout strategies (canary deployments, capacity-aware routing).
use octoroute::config::Config;
use octoroute::metrics::Metrics;
use octoroute::models::EndpointName;
use octoroute::models::selector::ModelSelector;
use octoroute::router::TargetModel;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Test that weighted selection respects 1:9 weight ratio
///
/// SCENARIO: Two endpoints with weights 1.0 and 9.0
///
/// EXPECTED: Over 1000 selections, endpoint-b (weight=9.0) should be selected
/// ~90% of the time (±5% variance allowed for randomness).
#[tokio::test]
async fn test_weighted_selection_respects_ratio() {
    // ARRANGE: Create config with 1:9 weight ratio
    let toml = r#"
        [server]
        host = "127.0.0.1"
        port = 3000

        [[models.fast]]
        name = "fast-light"
        base_url = "http://localhost:11434/v1"
        max_tokens = 4096
        weight = 1.0  # 10% of traffic

        [[models.fast]]
        name = "fast-heavy"
        base_url = "http://localhost:11435/v1"
        max_tokens = 4096
        weight = 9.0  # 90% of traffic

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

    // ACT: Select 1000 times and count frequency
    let mut counts: HashMap<String, usize> = HashMap::new();
    let exclusions: HashSet<EndpointName> = HashSet::new();

    for _ in 0..1000 {
        let selected = selector
            .select(TargetModel::Fast, &exclusions)
            .await
            .expect("Should select an endpoint");

        *counts.entry(selected.name().to_string()).or_insert(0) += 1;
    }

    // ASSERT: Verify distribution matches weights (±5% tolerance)
    let light_count = counts.get("fast-light").copied().unwrap_or(0);
    let heavy_count = counts.get("fast-heavy").copied().unwrap_or(0);

    let light_percentage = (light_count as f64 / 1000.0) * 100.0;
    let heavy_percentage = (heavy_count as f64 / 1000.0) * 100.0;

    println!(
        "Distribution: fast-light={:.1}% ({}/1000), fast-heavy={:.1}% ({}/1000)",
        light_percentage, light_count, heavy_percentage, heavy_count
    );

    // Expected: light ~10%, heavy ~90%
    // Allow ±5% variance (95% confidence interval for binomial distribution)
    assert!(
        (5.0..=15.0).contains(&light_percentage),
        "fast-light should get ~10% of traffic (±5%), got {:.1}%",
        light_percentage
    );
    assert!(
        (85.0..=95.0).contains(&heavy_percentage),
        "fast-heavy should get ~90% of traffic (±5%), got {:.1}%",
        heavy_percentage
    );
}

/// Test that equal weights produce uniform distribution
///
/// SCENARIO: Three endpoints with equal weights (1.0 each)
///
/// EXPECTED: Each endpoint should receive ~33% of traffic (±8% variance)
#[tokio::test]
async fn test_equal_weights_produce_uniform_distribution() {
    // ARRANGE: Create config with equal weights
    let toml = r#"
        [server]
        host = "127.0.0.1"
        port = 3000

        [[models.fast]]
        name = "fast-1"
        base_url = "http://localhost:11434/v1"
        max_tokens = 4096
        weight = 1.0

        [[models.fast]]
        name = "fast-2"
        base_url = "http://localhost:11435/v1"
        max_tokens = 4096
        weight = 1.0

        [[models.fast]]
        name = "fast-3"
        base_url = "http://localhost:11436/v1"
        max_tokens = 4096
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

    // ACT: Select 1500 times and count frequency
    let mut counts: HashMap<String, usize> = HashMap::new();
    let exclusions: HashSet<EndpointName> = HashSet::new();

    for _ in 0..1500 {
        let selected = selector
            .select(TargetModel::Fast, &exclusions)
            .await
            .expect("Should select an endpoint");

        *counts.entry(selected.name().to_string()).or_insert(0) += 1;
    }

    // ASSERT: Verify each endpoint gets ~33% (±8% for 3-way split)
    for name in ["fast-1", "fast-2", "fast-3"] {
        let count = counts.get(name).copied().unwrap_or(0);
        let percentage = (count as f64 / 1500.0) * 100.0;

        println!("{}: {:.1}% ({}/1500)", name, percentage, count);

        // Expected: ~33.3%, allow ±8% variance for 3-way split
        assert!(
            (25.0..=42.0).contains(&percentage),
            "{} should get ~33% of traffic (±8%), got {:.1}%",
            name,
            percentage
        );
    }
}

/// Test that weight=0.1 vs weight=10.0 produces expected ratio
///
/// SCENARIO: Canary deployment with 1% traffic to canary
///
/// EXPECTED: Canary gets ~1% of traffic, stable gets ~99%
#[tokio::test]
async fn test_canary_deployment_weight_ratio() {
    // ARRANGE: Create config with canary deployment weights
    let toml = r#"
        [server]
        host = "127.0.0.1"
        port = 3000

        [[models.fast]]
        name = "fast-canary"
        base_url = "http://localhost:11434/v1"
        max_tokens = 4096
        weight = 0.1  # 1% of traffic

        [[models.fast]]
        name = "fast-stable"
        base_url = "http://localhost:11435/v1"
        max_tokens = 4096
        weight = 9.9  # 99% of traffic

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

    // ACT: Select 10000 times (larger sample for 1% measurement)
    let mut counts: HashMap<String, usize> = HashMap::new();
    let exclusions: HashSet<EndpointName> = HashSet::new();

    for _ in 0..10000 {
        let selected = selector
            .select(TargetModel::Fast, &exclusions)
            .await
            .expect("Should select an endpoint");

        *counts.entry(selected.name().to_string()).or_insert(0) += 1;
    }

    // ASSERT: Verify canary gets ~1% (±0.5% with large sample)
    let canary_count = counts.get("fast-canary").copied().unwrap_or(0);
    let stable_count = counts.get("fast-stable").copied().unwrap_or(0);

    let canary_percentage = (canary_count as f64 / 10000.0) * 100.0;
    let stable_percentage = (stable_count as f64 / 10000.0) * 100.0;

    println!(
        "Canary deployment: canary={:.2}% ({}/10000), stable={:.2}% ({}/10000)",
        canary_percentage, canary_count, stable_percentage, stable_count
    );

    // Expected: canary ~1%, stable ~99%
    assert!(
        (0.5..=1.5).contains(&canary_percentage),
        "fast-canary should get ~1% of traffic (±0.5%), got {:.2}%",
        canary_percentage
    );
    assert!(
        (98.5..=99.5).contains(&stable_percentage),
        "fast-stable should get ~99% of traffic (±0.5%), got {:.2}%",
        stable_percentage
    );
}
