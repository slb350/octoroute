//! GAP #6: Concurrent Routing Request Tests
//!
//! Tests that verify the router handles concurrent routing requests correctly.
//! Ensures thread safety and proper concurrent access patterns.

use octoroute::config::Config;
use octoroute::models::selector::ModelSelector;
use octoroute::router::hybrid::HybridRouter;
use octoroute::router::{Importance, RouteMetadata, TaskType};
use std::sync::Arc;

fn test_config() -> Arc<Config> {
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
router_model = "balanced"
"#;

    let config: Config = toml::from_str(config_toml).expect("should parse config");
    Arc::new(config)
}

#[tokio::test]
async fn test_concurrent_routing_requests_same_metadata() {
    // GAP #6: Concurrent routing with identical metadata
    //
    // Spawn multiple concurrent routing requests with the same metadata.
    // All should succeed and return the same routing decision.

    let config = test_config();
    let selector = Arc::new(ModelSelector::new(config.clone()));
    let router = Arc::new(HybridRouter::new(config, selector).expect("should create router"));

    // Metadata that will match rule-based routing (casual chat -> Fast)
    let meta = RouteMetadata {
        token_estimate: 100,
        importance: Importance::Low,
        task_type: TaskType::CasualChat,
    };

    // Spawn 20 concurrent routing requests
    let handles: Vec<_> = (0..20)
        .map(|i| {
            let router_clone = Arc::clone(&router);
            let meta_clone = meta;
            tokio::spawn(async move {
                router_clone
                    .route(&format!("Hello world {}", i), &meta_clone)
                    .await
            })
        })
        .collect();

    // All should succeed
    let mut results = Vec::new();
    for handle in handles {
        let result = handle.await.expect("task should not panic");
        assert!(result.is_ok(), "Routing should succeed");
        results.push(result.unwrap());
    }

    // All should return the same decision (Fast tier via rule-based routing)
    assert_eq!(results.len(), 20);
    for decision in &results {
        assert_eq!(
            decision.target(),
            octoroute::router::TargetModel::Fast,
            "All concurrent requests should route to Fast tier"
        );
        assert_eq!(
            decision.strategy(),
            octoroute::router::RoutingStrategy::Rule,
            "All should use rule-based routing"
        );
    }
}

#[tokio::test]
async fn test_concurrent_routing_requests_different_metadata() {
    // GAP #6: Concurrent routing with different metadata
    //
    // Spawn concurrent requests with varying metadata to test
    // that the router correctly handles different routing decisions concurrently.

    let config = test_config();
    let selector = Arc::new(ModelSelector::new(config.clone()));
    let router = Arc::new(HybridRouter::new(config, selector).expect("should create router"));

    // Create different metadata profiles
    let metadata_profiles = [
        // Profile 1: Casual chat (should route to Fast)
        RouteMetadata {
            token_estimate: 100,
            importance: Importance::Low,
            task_type: TaskType::CasualChat,
        },
        // Profile 2: Code task (should route to Balanced)
        RouteMetadata {
            token_estimate: 512,
            importance: Importance::Normal,
            task_type: TaskType::Code,
        },
        // Profile 3: High importance (should route to Deep)
        RouteMetadata {
            token_estimate: 1000,
            importance: Importance::High,
            task_type: TaskType::QuestionAnswer,
        },
    ];

    // Spawn concurrent requests with rotating metadata
    let handles: Vec<_> = (0..30)
        .map(|i| {
            let router_clone = Arc::clone(&router);
            let meta = metadata_profiles[i % 3];
            tokio::spawn(async move {
                let result = router_clone.route(&format!("Request {}", i), &meta).await;
                (i, result)
            })
        })
        .collect();

    // Collect results
    let mut results = Vec::new();
    for handle in handles {
        let (i, result) = handle.await.expect("task should not panic");
        assert!(result.is_ok(), "Request {} should succeed", i);
        results.push((i, result.unwrap()));
    }

    // Verify routing decisions match expected tiers
    for (i, decision) in results {
        let expected_tier = match i % 3 {
            0 => octoroute::router::TargetModel::Fast, // Casual chat
            1 => octoroute::router::TargetModel::Balanced, // Code
            2 => octoroute::router::TargetModel::Deep, // High importance
            _ => unreachable!(),
        };

        assert_eq!(
            decision.target(),
            expected_tier,
            "Request {} should route to {:?}",
            i,
            expected_tier
        );
    }
}

#[tokio::test]
async fn test_concurrent_routing_high_load() {
    // GAP #6: High concurrent load test
    //
    // Stress test with 100 concurrent routing requests to verify
    // the router handles high concurrency without panics or deadlocks.

    let config = test_config();
    let selector = Arc::new(ModelSelector::new(config.clone()));
    let router = Arc::new(HybridRouter::new(config, selector).expect("should create router"));

    let meta = RouteMetadata {
        token_estimate: 256,
        importance: Importance::Normal,
        task_type: TaskType::QuestionAnswer,
    };

    // Spawn 100 concurrent routing requests
    let handles: Vec<_> = (0..100)
        .map(|i| {
            let router_clone = Arc::clone(&router);
            let meta_clone = meta;
            tokio::spawn(async move {
                router_clone
                    .route(&format!("Concurrent request {}", i), &meta_clone)
                    .await
            })
        })
        .collect();

    // All should complete successfully without panics
    let mut success_count = 0;
    for handle in handles {
        let result = handle.await.expect("task should not panic");
        if result.is_ok() {
            success_count += 1;
        }
    }

    // All 100 requests should succeed
    assert_eq!(
        success_count, 100,
        "All 100 concurrent requests should succeed"
    );
}
